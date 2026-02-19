"""Certificate bundle for Google Cast device authentication.

Loads a go-cast compatible JSON manifest containing the TLS peer certificate,
device authentication certificate, intermediate CA chain, and pre-computed
signature needed to pass Cast device authentication.
"""

from __future__ import annotations

import base64
import hashlib
import json
from dataclasses import dataclass
from typing import TYPE_CHECKING

from cryptography.hazmat.primitives.serialization import Encoding
from cryptography.x509 import (
    load_pem_x509_certificate,
    load_pem_x509_certificates,
)

if TYPE_CHECKING:
    from pathlib import Path

#: Required keys in the certificate manifest JSON.
_REQUIRED_KEYS = frozenset({"pu", "pr", "cpu", "ica", "sig_sha1"})


@dataclass(slots=True)
class CertificateBundle:
    """All cryptographic material needed for Cast device authentication.

    Fields are stored in their wire-ready formats: PEM for TLS context setup,
    DER for the device auth protobuf response, and raw bytes for the
    pre-computed signature.
    """

    #: TLS server certificate (PEM).
    peer_cert_pem: bytes
    #: TLS server private key (PEM).
    peer_key_pem: bytes
    #: Manufacturing device certificate (DER).
    device_cert_der: bytes
    #: Intermediate CA chain, each certificate in DER.
    intermediate_certs_der: list[bytes]
    #: Pre-computed RSASSA-PKCS1v15 signature of ``SHA1(peer_cert_DER)``.
    signature_sha1: bytes
    #: Peer certificate in DER, computed from *peer_cert_pem* at load time.
    peer_cert_der: bytes

    @classmethod
    def from_manifest(cls, path: Path) -> CertificateBundle:
        """Load a certificate bundle from a go-cast JSON manifest.

        The manifest is a flat JSON object with string values:

        ============ ================================================
        Key          Description
        ============ ================================================
        ``pu``       Peer certificate PEM
        ``pr``       Peer private key PEM
        ``cpu``      Device authentication certificate PEM
        ``ica``      Intermediate CA certificate(s) PEM (may be
                     multiple concatenated PEM blocks)
        ``sig_sha1`` Base64-encoded SHA-1 signature
        ============ ================================================

        Raises:
            ValueError: If required keys are missing or PEM data is invalid.
        """
        raw = path.read_text(encoding="utf-8")
        manifest: dict[str, str] = json.loads(raw)

        missing = _REQUIRED_KEYS - manifest.keys()
        if missing:
            msg = f"Manifest missing required keys: {', '.join(sorted(missing))}"
            raise ValueError(msg)

        # PEM bytes ---------------------------------------------------------
        peer_cert_pem = manifest["pu"].encode()
        peer_key_pem = manifest["pr"].encode()

        # Peer certificate PEM -> DER --------------------------------------
        peer_cert = load_pem_x509_certificate(peer_cert_pem)
        peer_cert_der = peer_cert.public_bytes(Encoding.DER)

        # Device (client auth) certificate PEM -> DER ----------------------
        cpu_pem = manifest["cpu"].encode()
        device_cert = load_pem_x509_certificate(cpu_pem)
        device_cert_der = device_cert.public_bytes(Encoding.DER)

        # Intermediate CA certificates PEM -> list[DER] --------------------
        ica_pem = manifest["ica"].encode()
        ica_certs = load_pem_x509_certificates(ica_pem)
        intermediate_certs_der = [cert.public_bytes(Encoding.DER) for cert in ica_certs]

        # Signature ---------------------------------------------------------
        signature_sha1 = base64.b64decode(manifest["sig_sha1"])

        return cls(
            peer_cert_pem=peer_cert_pem,
            peer_key_pem=peer_key_pem,
            device_cert_der=device_cert_der,
            intermediate_certs_der=intermediate_certs_der,
            signature_sha1=signature_sha1,
            peer_cert_der=peer_cert_der,
        )

    @property
    def cert_digest_md5(self) -> str:
        """MD5 hex digest of *peer_cert_der*, used for the mDNS ``cd`` TXT field."""
        return hashlib.md5(self.peer_cert_der).hexdigest()  # noqa: S324
