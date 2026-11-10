"""Finite Makefile compatibility for unchanged native unit recipes.

The reviewed main transition adds the public media-runtime repository variable
and changes
only docker/docker-up recipes. The complete byte identities keep assignments,
overrides, includes and every other target closed to unreviewed drift.
"""
import hashlib

PRE_MEDIA_RUNTIME_MAKE = 'df9c99f78aab7649439e60cc376cf0796ee744ff19b39396ef78aad5a079a112'
MEDIA_RUNTIME_MAKE = 'c63779a7a7b97c301fc3cc6fcdc719265fd1d03445dbbbd5ea443c2287fd3fe2'
REVIEWED_NATIVE_MAKEFILES = frozenset({PRE_MEDIA_RUNTIME_MAKE, MEDIA_RUNTIME_MAKE})


def native_makefile_digest(raw):
    """Normalize only the two fully reviewed files; unknown bytes stay distinct."""
    actual = hashlib.sha256(raw).hexdigest()
    return PRE_MEDIA_RUNTIME_MAKE if actual in REVIEWED_NATIVE_MAKEFILES else actual


def reviewed_native_makefile_transition(before, after):
    """A protected-path exception requires both complete reviewed identities."""
    return (before is not None and after is not None
            and hashlib.sha256(before).hexdigest() in REVIEWED_NATIVE_MAKEFILES
            and hashlib.sha256(after).hexdigest() in REVIEWED_NATIVE_MAKEFILES)
