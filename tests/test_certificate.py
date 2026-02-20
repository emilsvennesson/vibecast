"""Tests for the CertificateBundle manifest loader."""

from __future__ import annotations

import base64
import hashlib
import json
from typing import TYPE_CHECKING, Any

import pytest
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa
from cryptography.x509 import (
    CertificateBuilder,
    Name,
    NameAttribute,
    load_der_x509_certificate,
    random_serial_number,
)
from cryptography.x509.oid import NameOID

from castvibe._certificate import CertificateBundle

if TYPE_CHECKING:
    from pathlib import Path

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _generate_key() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def _self_signed_cert(
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


def _build_manifest(
    peer_pem: bytes,
    peer_key_pem: bytes,
    device_pem: bytes,
    ica_pem: bytes,
    sig_sha1: bytes | None,
    sig: bytes | None = None,
    **extra: str,
) -> dict[str, str]:
    """Build a manifest dict matching the go-cast JSON format."""
    manifest: dict[str, Any] = {
        "pu": peer_pem.decode(),
        "pr": peer_key_pem.decode(),
        "cpu": device_pem.decode(),
        "ica": ica_pem.decode(),
    }
    if sig_sha1 is not None:
        manifest["sig_sha1"] = base64.b64encode(sig_sha1).decode()
    if sig is not None:
        manifest["sig"] = base64.b64encode(sig).decode()
    manifest.update(extra)
    return manifest


def _write_manifest(tmp_path: Path, manifest: dict[str, str]) -> Path:
    p = tmp_path / "manifest.json"
    _ = p.write_text(json.dumps(manifest), encoding="utf-8")
    return p


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def cert_material() -> dict[
    str,
    bytes | rsa.RSAPrivateKey,
]:
    """Generate all crypto material for a complete test manifest.

    Returns a dict with keys: peer_pem, peer_der, peer_key_pem,
    device_pem, device_der, device_key, ica1_pem, ica1_der,
    ica2_pem, ica2_der, sig_sha1.
    """
    # Peer (TLS) key pair
    peer_key = _generate_key()
    peer_pem, peer_der = _self_signed_cert(peer_key, "PeerCert")
    peer_key_pem = peer_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )

    # Device (manufacturing) key pair
    device_key = _generate_key()
    device_pem, device_der = _self_signed_cert(device_key, "DeviceCert")

    # Two ICA certificates
    ica1_key = _generate_key()
    ica1_pem, ica1_der = _self_signed_cert(ica1_key, "ICA1")

    ica2_key = _generate_key()
    ica2_pem, ica2_der = _self_signed_cert(ica2_key, "ICA2")

    # Compute a real SHA-1 signature: RSASSA-PKCS1v15(SHA1(peer_der))
    sig_sha1 = device_key.sign(
        peer_der,
        padding.PKCS1v15(),
        hashes.SHA1(),  # noqa: S303
    )

    return {
        "peer_pem": peer_pem,
        "peer_der": peer_der,
        "peer_key_pem": peer_key_pem,
        "device_pem": device_pem,
        "device_der": device_der,
        "device_key": device_key,
        "ica1_pem": ica1_pem,
        "ica1_der": ica1_der,
        "ica2_pem": ica2_pem,
        "ica2_der": ica2_der,
        "sig_sha1": sig_sha1,
    }


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestFromManifest:
    """Tests for CertificateBundle.from_manifest()."""

    def test_loads_all_fields(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """All manifest fields are loaded and converted correctly."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        assert bundle.peer_cert_pem == cert_material["peer_pem"]
        assert bundle.peer_key_pem == cert_material["peer_key_pem"]
        assert bundle.device_cert_der == cert_material["device_der"]
        assert bundle.signature_sha1 == cert_material["sig_sha1"]
        assert len(bundle.intermediate_certs_der) == 1
        assert bundle.intermediate_certs_der[0] == cert_material["ica1_der"]

    def test_peer_cert_der_computed(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """peer_cert_der is computed from peer_cert_pem at load time."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        assert bundle.peer_cert_der == cert_material["peer_der"]

    def test_der_is_valid(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """DER bytes produced by the bundle can be parsed back as valid X.509."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        # Should not raise.
        peer = load_der_x509_certificate(bundle.peer_cert_der)
        device = load_der_x509_certificate(bundle.device_cert_der)
        ica = load_der_x509_certificate(bundle.intermediate_certs_der[0])

        assert peer is not None
        assert device is not None
        assert ica is not None

    def test_multiple_ica_certs(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """Concatenated ICA PEM blocks are split into separate DER entries."""
        combined_ica = cert_material["ica1_pem"] + cert_material["ica2_pem"]
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=combined_ica,
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        assert len(bundle.intermediate_certs_der) == 2
        assert bundle.intermediate_certs_der[0] == cert_material["ica1_der"]
        assert bundle.intermediate_certs_der[1] == cert_material["ica2_der"]

    def test_missing_required_key_raises(self, tmp_path: Path) -> None:
        """Missing required manifest keys raise ValueError."""
        manifest = {"pu": "x", "pr": "x"}  # missing cpu, ica, signature
        path = _write_manifest(tmp_path, manifest)

        with pytest.raises(ValueError, match="missing required keys"):
            _ = CertificateBundle.from_manifest(path)

    def test_missing_single_key(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """A single missing key is reported clearly."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        del manifest["sig_sha1"]
        path = _write_manifest(tmp_path, manifest)

        with pytest.raises(ValueError, match="sig_sha1 or sig"):
            _ = CertificateBundle.from_manifest(path)

    def test_legacy_sig_is_accepted(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """Legacy ``sig`` manifests load with SHA-256 hash selection."""
        device_key = cert_material["device_key"]
        assert isinstance(device_key, rsa.RSAPrivateKey)
        legacy_sig = device_key.sign(
            cert_material["peer_der"],
            padding.PKCS1v15(),
            hashes.SHA256(),
        )
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=None,
            sig=legacy_sig,
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        assert bundle.signature_sha1 == legacy_sig
        assert bundle.signature_is_sha1 is False

    def test_extra_keys_ignored(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """Unknown keys in the manifest (including 'crl') are silently ignored."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
            crl=base64.b64encode(b"crl").decode(),
            sig=b"otherdata",
        )
        path = _write_manifest(tmp_path, manifest)

        # Should not raise — extra keys are simply ignored.
        bundle = CertificateBundle.from_manifest(path)
        assert bundle.peer_cert_pem == cert_material["peer_pem"]

    def test_crl_loaded_when_present(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """Base64 ``crl`` manifests are decoded into bundle.crl bytes."""
        crl = b"\x01\x02\x03\x04"
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
            crl=base64.b64encode(crl).decode(),
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        assert bundle.crl == crl


class TestCertDigestMd5:
    """Tests for the cert_digest_md5 property."""

    def test_returns_correct_hex(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """cert_digest_md5 matches MD5(peer_cert_der) as hex."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        expected = hashlib.md5(cert_material["peer_der"]).hexdigest()  # noqa: S324
        assert bundle.cert_digest_md5 == expected

    def test_is_32_hex_chars(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        """The digest is a 32-character lowercase hex string."""
        manifest = _build_manifest(
            peer_pem=cert_material["peer_pem"],
            peer_key_pem=cert_material["peer_key_pem"],
            device_pem=cert_material["device_pem"],
            ica_pem=cert_material["ica1_pem"],
            sig_sha1=cert_material["sig_sha1"],
        )
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        digest = bundle.cert_digest_md5
        assert len(digest) == 32
        assert digest == digest.lower()
        assert all(c in "0123456789abcdef" for c in digest)
