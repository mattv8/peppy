"""Native CoreFoundation resolution for serialized Finder aliases."""

from __future__ import annotations

import ctypes
import os
import sys
from pathlib import Path

CORE_FOUNDATION = "/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation"
WITHOUT_UI = 1 << 8
WITHOUT_MOUNTING = 1 << 9


def resolve_alias(alias_bytes: bytes) -> Path:
    """Resolve a Finder Alias record without UI or volume mounting."""
    if sys.platform != "darwin":
        raise RuntimeError("CoreFoundation alias resolution requires macOS")

    core_foundation = ctypes.CDLL(CORE_FOUNDATION)
    core_foundation.CFDataCreate.argtypes = (ctypes.c_void_p, ctypes.c_void_p, ctypes.c_long)
    core_foundation.CFDataCreate.restype = ctypes.c_void_p
    core_foundation.CFURLCreateBookmarkDataFromAliasRecord.argtypes = (ctypes.c_void_p, ctypes.c_void_p)
    core_foundation.CFURLCreateBookmarkDataFromAliasRecord.restype = ctypes.c_void_p
    core_foundation.CFURLCreateByResolvingBookmarkData.argtypes = (
        ctypes.c_void_p, ctypes.c_void_p, ctypes.c_ulong, ctypes.c_void_p, ctypes.c_void_p,
        ctypes.POINTER(ctypes.c_ubyte), ctypes.POINTER(ctypes.c_void_p),
    )
    core_foundation.CFURLCreateByResolvingBookmarkData.restype = ctypes.c_void_p
    core_foundation.CFURLGetFileSystemRepresentation.argtypes = (
        ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_void_p, ctypes.c_long,
    )
    core_foundation.CFURLGetFileSystemRepresentation.restype = ctypes.c_ubyte
    core_foundation.CFRelease.argtypes = (ctypes.c_void_p,)
    core_foundation.CFRelease.restype = None

    alias_buffer = ctypes.create_string_buffer(alias_bytes)
    alias_data = core_foundation.CFDataCreate(None, alias_buffer, len(alias_bytes))
    bookmark = None
    resolved = None
    error = ctypes.c_void_p()
    try:
        if not alias_data:
            raise RuntimeError("could not create CoreFoundation alias data")
        bookmark = core_foundation.CFURLCreateBookmarkDataFromAliasRecord(None, alias_data)
        if not bookmark:
            raise RuntimeError("could not convert Alias record to bookmark data")
        stale = ctypes.c_ubyte()
        resolved = core_foundation.CFURLCreateByResolvingBookmarkData(
            None, bookmark, WITHOUT_UI | WITHOUT_MOUNTING, None, None, ctypes.byref(stale), ctypes.byref(error),
        )
        if not resolved:
            raise RuntimeError("could not resolve Alias record")
        buffer = ctypes.create_string_buffer(os.pathconf("/", "PC_PATH_MAX"))
        if not core_foundation.CFURLGetFileSystemRepresentation(resolved, True, buffer, len(buffer)):
            raise RuntimeError("could not obtain resolved filesystem path")
        return Path(os.fsdecode(buffer.value))
    finally:
        for reference in (resolved, bookmark, alias_data, error.value):
            if reference:
                core_foundation.CFRelease(reference)
