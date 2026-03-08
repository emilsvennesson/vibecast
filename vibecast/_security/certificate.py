"""Certificate material loading for Google Cast TLS and device auth.

The manifest stores one manufacturing/device certificate chain and many peer
certificates (each with SHA-1 and SHA-256 auth signatures).  A
``CertificateStore`` selects the currently valid peer certificate and can
rotate automatically as certificates expire.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import json
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import TYPE_CHECKING, cast

from cryptography.hazmat.primitives.serialization import Encoding
from cryptography.x509 import (
    load_pem_x509_certificate,
    load_pem_x509_certificates,
)

if TYPE_CHECKING:
    from pathlib import Path


_REQUIRED_SHARED_KEYS = frozenset({"cpu", "ica", "certs"})
_REQUIRED_CERT_KEYS = frozenset({"pu", "pr", "sig_sha1", "sig_sha256"})


def _require_str(value: object, *, key: str, context: str) -> str:
    if not isinstance(value, str) or not value:
        msg = f"{context} missing required string key: {key}"
        raise ValueError(msg)
    return value


def _decode_b64(value: str, *, key: str, context: str) -> bytes:
    try:
        return base64.b64decode(value, validate=True)
    except binascii.Error as exc:
        msg = f"{context} contains invalid base64 in key: {key}"
        raise ValueError(msg) from exc


def _to_utc(ts: datetime) -> datetime:
    if ts.tzinfo is None:
        return ts.replace(tzinfo=UTC)
    return ts.astimezone(UTC)


def _utc_now() -> datetime:
    return datetime.now(UTC)


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
    #: Pre-computed static auth signature over SHA1(peer_cert_der).
    signature_sha1: bytes
    #: Pre-computed static auth signature over SHA256(peer_cert_der).
    signature_sha256: bytes
    #: Peer certificate in DER, computed from *peer_cert_pem* at load time.
    peer_cert_der: bytes
    #: Peer certificate validity start time (UTC).
    not_valid_before: datetime
    #: Peer certificate validity end time (UTC).
    not_valid_after: datetime
    #: Optional CRL payload to include in auth responses.
    crl: bytes | None = None

    @classmethod
    def from_manifest(cls, path: Path) -> CertificateBundle:
        """Load the currently active certificate from a manifest.

        This convenience API is equivalent to:

        ``CertificateStore.from_manifest(path).active_bundle``
        """
        return CertificateStore.from_manifest(path).active_bundle

    def is_valid_at(self, ts: datetime) -> bool:
        """Return whether this peer certificate is valid at *ts* (UTC)."""
        moment = _to_utc(ts)
        return self.not_valid_before <= moment <= self.not_valid_after

    @property
    def is_valid(self) -> bool:
        """Whether this peer certificate is currently valid."""
        return self.is_valid_at(_utc_now())

    @property
    def cert_digest_md5(self) -> str:
        """MD5 hex digest of *peer_cert_der*, used for the mDNS ``cd`` TXT field."""
        return hashlib.md5(self.peer_cert_der).hexdigest()  # noqa: S324


class CertificateStore:
    """Collection of peer certificates with active-certificate selection."""

    __slots__ = ("_active_index", "_bundles")

    def __init__(self, bundles: list[CertificateBundle]) -> None:
        if not bundles:
            msg = "Certificate manifest must contain at least one certificate"
            raise ValueError(msg)
        ordered = sorted(bundles, key=lambda item: item.not_valid_before)
        self._bundles = tuple(ordered)

        active_index = self._find_valid_index(_utc_now())
        if active_index is None:
            msg = "Certificate manifest does not contain a currently valid certificate"
            raise ValueError(msg)
        self._active_index = active_index

    @property
    def bundles(self) -> tuple[CertificateBundle, ...]:
        """All loaded peer certificates sorted by validity start time."""
        return self._bundles

    @property
    def active_bundle(self) -> CertificateBundle:
        """Currently selected valid certificate bundle."""
        return self._bundles[self._active_index]

    def _find_valid_index(self, ts: datetime) -> int | None:
        for index, bundle in enumerate(self._bundles):
            if bundle.is_valid_at(ts):
                return index
        return None

    def rotate_if_needed(
        self, *, now: datetime | None = None
    ) -> CertificateBundle | None:
        """Rotate to a new valid certificate when the active one expires.

        Returns the newly selected bundle when a rotation happened, otherwise
        ``None``.
        """
        moment = _utc_now() if now is None else _to_utc(now)
        current = self.active_bundle
        if current.is_valid_at(moment):
            return None

        active_index = self._find_valid_index(moment)
        if active_index is None:
            msg = "No valid peer certificate found for the current time"
            raise RuntimeError(msg)

        if active_index == self._active_index:
            return None

        self._active_index = active_index
        return self.active_bundle

    @classmethod
    def from_bundle(cls, bundle: CertificateBundle) -> CertificateStore:
        """Build a store from a single preconstructed bundle."""
        return cls([bundle])

    @classmethod
    def from_manifest(cls, path: Path) -> CertificateStore:
        """Load a certificate store from manifest JSON.

        Required format:
        ``{"cpu": ..., "ica": ..., "certs": [{...}, ...]}``
        """
        raw = path.read_text(encoding="utf-8")
        payload_raw = json.loads(raw)
        if not isinstance(payload_raw, dict):
            msg = "Manifest root must be a JSON object"
            raise TypeError(msg)

        payload_dict = cast("dict[object, object]", payload_raw)
        payload: dict[str, object] = {}
        for key, value in payload_dict.items():
            if not isinstance(key, str):
                msg = "Manifest keys must be strings"
                raise TypeError(msg)
            payload[key] = value

        missing_shared = _REQUIRED_SHARED_KEYS - payload.keys()
        if missing_shared:
            msg = (
                "Manifest missing required shared keys: "
                f"{', '.join(sorted(missing_shared))}"
            )
            raise ValueError(msg)

        cpu_pem = _require_str(payload.get("cpu"), key="cpu", context="manifest")
        ica_pem = _require_str(payload.get("ica"), key="ica", context="manifest")

        device_cert = load_pem_x509_certificate(cpu_pem.encode("utf-8"))
        device_cert_der = device_cert.public_bytes(Encoding.DER)

        ica_certs = load_pem_x509_certificates(ica_pem.encode("utf-8"))
        intermediate_certs_der = [cert.public_bytes(Encoding.DER) for cert in ica_certs]

        crl_b64 = payload.get("crl")
        if crl_b64 is None:
            crl = None
        else:
            crl_str = _require_str(crl_b64, key="crl", context="manifest")
            crl = _decode_b64(crl_str, key="crl", context="manifest")

        cert_entries_raw = payload.get("certs")
        cert_dicts: list[dict[str, object]]
        entry_contexts: list[str]
        if not isinstance(cert_entries_raw, list) or not cert_entries_raw:
            msg = "Manifest key 'certs' must be a non-empty list"
            raise ValueError(msg)

        cert_entries = cast("list[object]", cert_entries_raw)

        cert_dicts = []
        entry_contexts = []
        for index, entry in enumerate(cert_entries):
            context = f"manifest.certs[{index}]"
            if not isinstance(entry, dict):
                msg = f"{context} must be a JSON object"
                raise TypeError(msg)

            entry_dict = cast("dict[object, object]", entry)
            typed_entry: dict[str, object] = {}
            for key, value in entry_dict.items():
                if not isinstance(key, str):
                    msg = f"{context} keys must be strings"
                    raise TypeError(msg)
                typed_entry[key] = value

            cert_dicts.append(typed_entry)
            entry_contexts.append(context)

        bundles: list[CertificateBundle] = []
        for item, context in zip(cert_dicts, entry_contexts, strict=True):
            missing_cert = _REQUIRED_CERT_KEYS - item.keys()
            if missing_cert:
                msg = (
                    f"{context} missing required keys: "
                    f"{', '.join(sorted(missing_cert))}"
                )
                raise ValueError(msg)

            peer_cert_pem = _require_str(
                item.get("pu"), key="pu", context=context
            ).encode("utf-8")
            peer_key_pem = _require_str(
                item.get("pr"), key="pr", context=context
            ).encode("utf-8")
            sig_sha1_b64 = _require_str(
                item.get("sig_sha1"), key="sig_sha1", context=context
            )
            sig_sha256_b64 = _require_str(
                item.get("sig_sha256"), key="sig_sha256", context=context
            )

            peer_cert = load_pem_x509_certificate(peer_cert_pem)
            peer_cert_der = peer_cert.public_bytes(Encoding.DER)

            not_valid_before = _to_utc(peer_cert.not_valid_before_utc)
            not_valid_after = _to_utc(peer_cert.not_valid_after_utc)

            bundles.append(
                CertificateBundle(
                    peer_cert_pem=peer_cert_pem,
                    peer_key_pem=peer_key_pem,
                    device_cert_der=device_cert_der,
                    intermediate_certs_der=list(intermediate_certs_der),
                    signature_sha1=_decode_b64(
                        sig_sha1_b64,
                        key="sig_sha1",
                        context=context,
                    ),
                    signature_sha256=_decode_b64(
                        sig_sha256_b64,
                        key="sig_sha256",
                        context=context,
                    ),
                    peer_cert_der=peer_cert_der,
                    not_valid_before=not_valid_before,
                    not_valid_after=not_valid_after,
                    crl=crl,
                )
            )

        return cls(bundles)
