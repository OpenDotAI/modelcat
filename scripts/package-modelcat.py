#!/usr/bin/env python3
"""Package a client with executable modes intact; signing never bypasses Gatekeeper."""
import argparse, hashlib, json, os, plistlib, shutil, subprocess, sys, tarfile, tempfile, zipfile
from pathlib import Path


def zip_tree(source, output):
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob('*')):
            if path.is_file():
                info = zipfile.ZipInfo.from_file(path, path.relative_to(source).as_posix())
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, path.read_bytes())


def package(binary, target, version, output, identity=None, notary=None):
    if not binary.is_file():
        raise ValueError('Build the modelcat binary first')
    output.mkdir(parents=True, exist_ok=True)
    if not version or any(c not in '0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ.-+' for c in version):
        raise ValueError('Invalid release version')
    with tempfile.TemporaryDirectory(prefix='modelcat-package-') as directory:
        stage = Path(directory)
        executable = stage / 'modelcat'
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        (stage / 'README.txt').write_text('ModelCat ' + version + '\nRun ./modelcat start, then ./modelcat open.\nmacOS: extract ModelCat.app and move it into Applications before opening.\nThe first unsigned release requires macOS approval. No sudo or manual chmod is needed.\nCredentials are imported only after an explicit action in the local UI.\nhttps://github.com/OpenDotAI/modelcat\n')
        files = []
        signing = 'not-applicable'
        if 'apple-darwin' in target:
            if sys.platform != 'darwin':
                raise ValueError('macOS bundles must be signed and packaged on macOS')
            app = stage / 'ModelCat.app'
            macos = app / 'Contents/MacOS'
            macos.mkdir(parents=True)
            shutil.copyfile(executable, macos / 'modelcat')
            (macos / 'modelcat').chmod(0o755)
            plist = {'CFBundleIdentifier': 'ai.opendot.modelcat', 'CFBundleName': 'ModelCat', 'CFBundleDisplayName': 'ModelCat', 'CFBundleExecutable': 'modelcat', 'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': version.split('-')[0].split('+')[0], 'CFBundleVersion': version, 'LSMinimumSystemVersion': '12.0', 'LSUIElement': True, 'NSHighResolutionCapable': True}
            (app / 'Contents/Info.plist').write_bytes(plistlib.dumps(plist))
            sign_args = ['codesign', '--force', '--sign', identity or '-']
            if identity:
                sign_args += ['--options', 'runtime', '--timestamp']
            subprocess.run(sign_args + [str(app)], check=True)
            subprocess.run(['codesign', '--verify', '--strict', str(app)], check=True)
            signing = 'developer-id' if identity else 'ad-hoc; not notarized'
            if notary:
                if not identity:
                    raise ValueError('Notarization requires a Developer ID identity')
                notarize_zip = stage / 'notary.zip'
                subprocess.run(['ditto', '-c', '-k', '--keepParent', str(app), str(notarize_zip)], check=True)
                subprocess.run(['xcrun', 'notarytool', 'submit', str(notarize_zip), '--keychain-profile', notary, '--wait'], check=True)
                subprocess.run(['xcrun', 'stapler', 'staple', str(app)], check=True)
                notarize_zip.unlink()
                signing = 'developer-id; notarized'
            app_zip = output / f'modelcat-{target}.zip'
            zip_tree(stage, app_zip)
            files.append(app_zip)
            dmg_root = stage / 'disk'
            dmg_root.mkdir()
            shutil.copytree(app, dmg_root / 'ModelCat.app')
            (dmg_root / 'Applications').symlink_to('/Applications')
            dmg = output / f'modelcat-{target}.dmg'
            subprocess.run(['hdiutil', 'create', '-quiet', '-volname', 'ModelCat', '-srcfolder', str(dmg_root), '-ov', '-format', 'UDZO', str(dmg)], check=True)
            if identity:
                subprocess.run(['codesign', '--sign', identity, '--timestamp', str(dmg)], check=True)
            if notary:
                subprocess.run(['xcrun', 'notarytool', 'submit', str(dmg), '--keychain-profile', notary, '--wait'], check=True)
                subprocess.run(['xcrun', 'stapler', 'staple', str(dmg)], check=True)
            files.append(dmg)
        else:
            archive = output / f'modelcat-{target}.tar.gz'
            with tarfile.open(archive, 'w:gz') as tar:
                tar.add(executable, arcname='modelcat')
                tar.add(stage / 'README.txt', arcname='README.txt')
            files.append(archive)
        metadata = {'version': version, 'target': target, 'signing': signing, 'files': [{'name': p.name, 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in files]}
        (output / f'modelcat-{target}.json').write_text(json.dumps(metadata, indent=2) + '\n')
        return metadata


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--target', required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--output', type=Path, default=Path('release'))
    p.add_argument('--sign-identity', default=os.environ.get('MODELCAT_SIGN_IDENTITY'))
    p.add_argument('--notary-profile', default=os.environ.get('MODELCAT_NOTARY_PROFILE'))
    args = p.parse_args()
    print(json.dumps(package(args.binary, args.target, args.version.removeprefix('v'), args.output, args.sign_identity, args.notary_profile), indent=2))
