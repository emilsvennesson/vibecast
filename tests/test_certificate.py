"""Tests for certificate manifest loading and rotation."""

from __future__ import annotations

import base64
import datetime
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
    random_serial_number,
)
from cryptography.x509.oid import NameOID

from vibecast._certificate import CertificateBundle, CertificateStore

if TYPE_CHECKING:
    from pathlib import Path


def _generate_key() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


def _self_signed_cert(
    key: rsa.RSAPrivateKey,
    cn: str,
    *,
    not_before: datetime.datetime,
    not_after: datetime.datetime,
) -> tuple[bytes, bytes]:
    subject = issuer = Name([NameAttribute(NameOID.COMMON_NAME, cn)])
    cert = (
        CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(key.public_key())
        .serial_number(random_serial_number())
        .not_valid_before(not_before)
        .not_valid_after(not_after)
        .sign(key, hashes.SHA256())
    )
    return (
        cert.public_bytes(serialization.Encoding.PEM),
        cert.public_bytes(serialization.Encoding.DER),
    )


def _b64(raw: bytes) -> str:
    return base64.b64encode(raw).decode("utf-8")


def _build_cert_entry(
    *,
    peer_pem: bytes,
    peer_key_pem: bytes,
    sig_sha1: bytes,
    sig_sha256: bytes,
) -> dict[str, str]:
    return {
        "pu": peer_pem.decode("utf-8"),
        "pr": peer_key_pem.decode("utf-8"),
        "sig_sha1": _b64(sig_sha1),
        "sig_sha256": _b64(sig_sha256),
    }


def _write_manifest(tmp_path: Path, manifest: dict[str, Any]) -> Path:
    path = tmp_path / "manifest.json"
    _ = path.write_text(json.dumps(manifest), encoding="utf-8")
    return path


@pytest.fixture
def cert_material() -> dict[str, Any]:
    now = datetime.datetime.now(datetime.UTC).replace(microsecond=0)

    device_key = _generate_key()
    device_pem, _device_der = _self_signed_cert(
        device_key,
        "DeviceCert",
        not_before=now - datetime.timedelta(days=30),
        not_after=now + datetime.timedelta(days=365),
    )

    ica1_key = _generate_key()
    ica1_pem, ica1_der = _self_signed_cert(
        ica1_key,
        "ICA1",
        not_before=now - datetime.timedelta(days=30),
        not_after=now + datetime.timedelta(days=365),
    )

    ica2_key = _generate_key()
    ica2_pem, ica2_der = _self_signed_cert(
        ica2_key,
        "ICA2",
        not_before=now - datetime.timedelta(days=30),
        not_after=now + datetime.timedelta(days=365),
    )

    current_peer_key = _generate_key()
    current_peer_pem, current_peer_der = _self_signed_cert(
        current_peer_key,
        "PeerCurrent",
        not_before=now - datetime.timedelta(hours=1),
        not_after=now + datetime.timedelta(hours=1),
    )
    current_peer_key_pem = current_peer_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )

    next_peer_key = _generate_key()
    next_peer_pem, next_peer_der = _self_signed_cert(
        next_peer_key,
        "PeerNext",
        not_before=now + datetime.timedelta(hours=1),
        not_after=now + datetime.timedelta(days=2),
    )
    next_peer_key_pem = next_peer_key.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.TraditionalOpenSSL,
        serialization.NoEncryption(),
    )

    return {
        "now": now,
        "device_pem": device_pem,
        "ica_pem": ica1_pem + ica2_pem,
        "ica1_der": ica1_der,
        "ica2_der": ica2_der,
        "current_peer_pem": current_peer_pem,
        "current_peer_der": current_peer_der,
        "current_peer_key_pem": current_peer_key_pem,
        "current_sig_sha1": device_key.sign(
            current_peer_der,
            padding.PKCS1v15(),
            hashes.SHA1(),  # noqa: S303
        ),
        "current_sig_sha256": device_key.sign(
            current_peer_der,
            padding.PKCS1v15(),
            hashes.SHA256(),
        ),
        "next_peer_pem": next_peer_pem,
        "next_peer_der": next_peer_der,
        "next_peer_key_pem": next_peer_key_pem,
        "next_sig_sha1": device_key.sign(
            next_peer_der,
            padding.PKCS1v15(),
            hashes.SHA1(),  # noqa: S303
        ),
        "next_sig_sha256": device_key.sign(
            next_peer_der,
            padding.PKCS1v15(),
            hashes.SHA256(),
        ),
    }


class TestFromManifest:
    def test_manifest_loads_active_bundle(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                )
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        store = CertificateStore.from_manifest(path)
        bundle = store.active_bundle

        assert bundle.peer_cert_pem == cert_material["current_peer_pem"]
        assert bundle.peer_key_pem == cert_material["current_peer_key_pem"]
        assert bundle.signature_sha1 == cert_material["current_sig_sha1"]
        assert bundle.signature_sha256 == cert_material["current_sig_sha256"]
        assert len(bundle.intermediate_certs_der) == 2
        assert bundle.intermediate_certs_der[0] == cert_material["ica1_der"]
        assert bundle.intermediate_certs_der[1] == cert_material["ica2_der"]

    def test_multi_manifest_selects_currently_valid_certificate(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        # Intentionally place future certificate first; loader should select by
        # validity, not list position.
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["next_peer_pem"],
                    peer_key_pem=cert_material["next_peer_key_pem"],
                    sig_sha1=cert_material["next_sig_sha1"],
                    sig_sha256=cert_material["next_sig_sha256"],
                ),
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                ),
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        store = CertificateStore.from_manifest(path)

        assert len(store.bundles) == 2
        assert store.active_bundle.peer_cert_der == cert_material["current_peer_der"]

    def test_convenience_loader_returns_active_bundle(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                )
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        bundle = CertificateBundle.from_manifest(path)

        assert bundle.peer_cert_der == cert_material["current_peer_der"]

    def test_rotate_if_needed_selects_next_certificate(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                ),
                _build_cert_entry(
                    peer_pem=cert_material["next_peer_pem"],
                    peer_key_pem=cert_material["next_peer_key_pem"],
                    sig_sha1=cert_material["next_sig_sha1"],
                    sig_sha256=cert_material["next_sig_sha256"],
                ),
            ],
        }
        path = _write_manifest(tmp_path, manifest)
        store = CertificateStore.from_manifest(path)

        rotated = store.rotate_if_needed(
            now=cert_material["now"] + datetime.timedelta(hours=1, minutes=5)
        )

        assert rotated is not None
        assert rotated.peer_cert_der == cert_material["next_peer_der"]
        assert store.active_bundle.peer_cert_der == cert_material["next_peer_der"]

    def test_missing_required_shared_key_raises(self, tmp_path: Path) -> None:
        path = _write_manifest(tmp_path, {"cpu": "pem"})

        with pytest.raises(ValueError, match="missing required shared keys"):
            _ = CertificateStore.from_manifest(path)

    def test_missing_required_cert_key_raises(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                {
                    "pu": cert_material["current_peer_pem"].decode("utf-8"),
                    "pr": cert_material["current_peer_key_pem"].decode("utf-8"),
                    "sig_sha1": _b64(cert_material["current_sig_sha1"]),
                }
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        with pytest.raises(ValueError, match="missing required keys"):
            _ = CertificateStore.from_manifest(path)

    def test_invalid_base64_raises(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                {
                    "pu": cert_material["current_peer_pem"].decode("utf-8"),
                    "pr": cert_material["current_peer_key_pem"].decode("utf-8"),
                    "sig_sha1": "not-base64",
                    "sig_sha256": _b64(cert_material["current_sig_sha256"]),
                }
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        with pytest.raises(ValueError, match="invalid base64"):
            _ = CertificateStore.from_manifest(path)

    def test_crl_loaded_when_present(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        crl = b"\x01\x02\x03\x04"
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "crl": _b64(crl),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                )
            ],
        }
        path = _write_manifest(tmp_path, manifest)

        store = CertificateStore.from_manifest(path)

        assert store.active_bundle.crl == crl


class TestCertDigestMd5:
    def test_returns_correct_hex(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                )
            ],
        }
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        expected = hashlib.md5(cert_material["current_peer_der"]).hexdigest()  # noqa: S324
        assert bundle.cert_digest_md5 == expected

    def test_is_32_hex_chars(
        self,
        tmp_path: Path,
        cert_material: dict[str, Any],
    ) -> None:
        manifest = {
            "cpu": cert_material["device_pem"].decode("utf-8"),
            "ica": cert_material["ica_pem"].decode("utf-8"),
            "certs": [
                _build_cert_entry(
                    peer_pem=cert_material["current_peer_pem"],
                    peer_key_pem=cert_material["current_peer_key_pem"],
                    sig_sha1=cert_material["current_sig_sha1"],
                    sig_sha256=cert_material["current_sig_sha256"],
                )
            ],
        }
        path = _write_manifest(tmp_path, manifest)
        bundle = CertificateBundle.from_manifest(path)

        digest = bundle.cert_digest_md5
        assert len(digest) == 32
        assert digest == digest.lower()
        assert all(c in "0123456789abcdef" for c in digest)
