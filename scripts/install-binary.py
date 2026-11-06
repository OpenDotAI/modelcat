#!/usr/bin/env python3
"""Replace a binary atomically, without rewriting a running executable inode."""
import os
from pathlib import Path
import shutil
import sys
import tempfile

source, destination = map(Path, sys.argv[1:])
destination.parent.mkdir(parents=True, exist_ok=True)
fd, temporary = tempfile.mkstemp(prefix=f".{destination.name}-", dir=destination.parent)
try:
    with os.fdopen(fd, "wb") as stream, source.open("rb") as incoming:
        shutil.copyfileobj(incoming, stream)
        stream.flush()
        os.fsync(stream.fileno())
    os.chmod(temporary, 0o755)
    os.replace(temporary, destination)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
