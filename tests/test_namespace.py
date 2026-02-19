"""Tests for Cast namespace URI constants."""

from castvibe import _namespace


def _all_namespace_values() -> list[str]:
    """Collect all public string constants from the namespace module."""
    return [
        getattr(_namespace, name)
        for name in dir(_namespace)
        if name.isupper() and isinstance(getattr(_namespace, name), str)
    ]


def test_all_namespaces_are_cast_urns() -> None:
    """Every namespace constant must start with 'urn:x-cast:'."""
    namespaces = _all_namespace_values()
    assert len(namespaces) > 0, "No namespace constants found"
    for ns in namespaces:
        assert ns.startswith("urn:x-cast:"), f"{ns!r} is not a valid Cast namespace"


def test_expected_namespaces_present() -> None:
    """The core platform namespaces required by the Cast protocol are defined."""
    assert _namespace.HEARTBEAT
    assert _namespace.CONNECTION
    assert _namespace.DEVICE_AUTH
    assert _namespace.RECEIVER
    assert _namespace.DISCOVERY
    assert _namespace.MEDIA
    assert _namespace.MULTIZONE
    assert _namespace.SETUP
