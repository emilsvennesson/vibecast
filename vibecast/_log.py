"""Logging helpers for vibecast."""

import logging


def get_logger(name: str) -> logging.Logger:
    """Return a logger under the ``vibecast`` namespace."""
    return logging.getLogger(f"vibecast.{name}")
