"""Logging helpers for castvibe."""

import logging


def get_logger(name: str) -> logging.Logger:
    """Return a logger under the ``castvibe`` namespace."""
    return logging.getLogger(f"castvibe.{name}")
