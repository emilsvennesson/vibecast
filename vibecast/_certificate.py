"""Certificate bundle for Google Cast device authentication.

Loads a go-cast compatible JSON manifest containing the TLS peer certificate,
device authentication certificate, intermediate CA chain, and a pre-computed
auth signature needed to pass Cast device authentication.
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

#: Required non-signature keys in the certificate manifest JSON.
_REQUIRED_KEYS = frozenset({"pu", "pr", "cpu", "ica"})


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
    #: Pre-computed static auth signature.
    #:
    #: The field name is historical: when ``signature_is_sha1`` is ``False``
    #: this stores the SHA-256 signature loaded from legacy ``sig`` manifests.
    signature_sha1: bytes
    #: Peer certificate in DER, computed from *peer_cert_pem* at load time.
    peer_cert_der: bytes
    #: Whether ``signature_sha1`` is for SHA-1 (vs SHA-256 when ``False``).
    signature_is_sha1: bool = True
    #: Optional CRL payload to include in auth responses.
    crl: bytes | None = None

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
        ``sig_sha1`` Base64-encoded SHA-1 signature (preferred)
        ``sig``      Base64-encoded SHA-256 signature (legacy fallback)
        ``crl``      Base64-encoded Cast CRL blob (optional)
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

        has_sig_sha1 = bool(manifest.get("sig_sha1"))
        has_sig = bool(manifest.get("sig"))
        if not has_sig_sha1 and not has_sig:
            msg = "Manifest missing required signature key: sig_sha1 or sig"
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
        signature_is_sha1 = has_sig_sha1
        signature_key = "sig_sha1" if has_sig_sha1 else "sig"
        signature_sha1 = base64.b64decode(manifest[signature_key])

        # Optional CRL ------------------------------------------------------
        crl_b64 = manifest.get("crl")
        crl = base64.b64decode(crl_b64) if crl_b64 else None

        return cls(
            peer_cert_pem=peer_cert_pem,
            peer_key_pem=peer_key_pem,
            device_cert_der=device_cert_der,
            intermediate_certs_der=intermediate_certs_der,
            signature_sha1=signature_sha1,
            peer_cert_der=peer_cert_der,
            signature_is_sha1=signature_is_sha1,
            crl=crl,
        )

    @property
    def cert_digest_md5(self) -> str:
        """MD5 hex digest of *peer_cert_der*, used for the mDNS ``cd`` TXT field."""
        return hashlib.md5(self.peer_cert_der).hexdigest()  # noqa: S324
