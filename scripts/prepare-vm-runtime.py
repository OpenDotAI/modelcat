#!/usr/bin/env python3
"""Prepare pinned, checksum-verified VM resources."""
import argparse
import hashlib
import posixpath
import re
from pathlib import Path
import tarfile
import tempfile
import shutil
import urllib.request

ROOT = Path(__file__).resolve().parents[1] / '.vm-tools'
IMAGE = ROOT / 'images' / 'ubuntu-arm64.img'
ARTIFACTS = [
    ('lima', 'https://github.com/lima-vm/lima/releases/download/v2.3.0/lima-2.3.0-Darwin-arm64.tar.gz', 'b7af444bd8f91d3c307477e562f2cf3bd7595d62b9429185e4fa841563578cfc'),
    ('guest', 'https://github.com/OpenDotAI/modelcat/releases/download/v0.2.0-build.2/modelcat-aarch64-unknown-linux-gnu.tar.gz', 'f2cfce70f5423e75cc4c709302f92f9470486d4055e5b83a33e8e5bedb5c65ac'),
]

def sha256_file(path):
    with path.open('rb') as source:
        checksum = hashlib.sha256()
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            checksum.update(chunk)
    return checksum.hexdigest()


def prepare_image(local_source):
    config = (ROOT.parent / 'apps/seller/vm.yaml').read_text()
    pinned = re.search(r'^images:\n- location: (https://\S+)\n  arch: aarch64\n  digest: sha256:([0-9a-f]{64})$', config, re.MULTILINE)
    if not pinned:
        raise ValueError('Missing pinned ARM64 image in apps/seller/vm.yaml')
    url, expected = pinned.groups()
    if IMAGE.is_file() and sha256_file(IMAGE) == expected:
        print('Ubuntu VM image: already verified')
        return
    IMAGE.parent.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix='.image-', dir=IMAGE.parent) as stage:
        temporary = Path(stage) / IMAGE.name
        if local_source is None:
            with urllib.request.urlopen(url, timeout=60) as source, temporary.open('wb') as output:
                shutil.copyfileobj(source, output)
        else:
            shutil.copyfile(local_source, temporary)
        if sha256_file(temporary) != expected:
            raise ValueError('Ubuntu VM image checksum mismatch')
        temporary.replace(IMAGE)
    print('Ubuntu VM image: verified ' + expected)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--image', action='store_true', help='also prepare the pinned Ubuntu image')
    parser.add_argument('--image-from', type=Path, help='prepare the pinned Ubuntu image from an existing copy')
    args = parser.parse_args()
    ROOT.mkdir(exist_ok=True)
    for name, url, expected in ARTIFACTS:
        archive = ROOT / (name + '.tar.gz')
        if not archive.exists():
            partial = archive.with_suffix('.partial')
            urllib.request.urlretrieve(url, partial)
            partial.replace(archive)
        if sha256_file(archive) != expected:
            raise ValueError('Checksum mismatch: ' + str(archive))
        destination = ROOT / name
        if (destination / '.verified-sha256').is_file() and (destination / '.verified-sha256').read_text() == expected:
            print(name + ': already prepared')
            continue
        with tempfile.TemporaryDirectory(prefix='.prepare-', dir=ROOT) as stage:
            staging = Path(stage) / name
            staging.mkdir()
            with tarfile.open(archive) as bundle:
                for item in bundle.getmembers():
                    parts = Path(item.name)
                    if parts.is_absolute() or '..' in parts.parts or item.isdev():
                        raise ValueError('Unsafe archive member')
                    if item.issym() or item.islnk():
                        target = Path(posixpath.normpath(posixpath.join(str(parts.parent) if item.issym() else '', item.linkname)))
                        if target.is_absolute() or '..' in target.parts:
                            raise ValueError('Unsafe archive link')
                bundle.extractall(staging)
            (staging / '.verified-sha256').write_text(expected)
            # New inodes avoid macOS code-signature cache invalidation from in-place writes.
            previous = ROOT / (name + '.previous')
            if previous.exists():
                raise ValueError('Previous runtime still retained: ' + str(previous))
            if destination.exists():
                destination.rename(previous)
            staging.rename(destination)
        print(name + ': verified ' + expected)
    if args.image or args.image_from is not None:
        prepare_image(args.image_from)

if __name__ == '__main__':
    main()
