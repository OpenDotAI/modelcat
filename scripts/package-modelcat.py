#!/usr/bin/env python3
"""Package a client with executable modes intact; signing never bypasses Gatekeeper."""
import argparse, hashlib, json, os, plistlib, shutil, subprocess, sys, tarfile, tempfile, zipfile
from pathlib import Path


def zip_tree(source, output):
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        for path in sorted(source.rglob('*')):
            if path.is_file() or path.is_symlink():
                info = zipfile.ZipInfo(path.relative_to(source).as_posix()) if path.is_symlink() else zipfile.ZipInfo.from_file(path, path.relative_to(source).as_posix())
                info.compress_type = zipfile.ZIP_DEFLATED
                if path.is_symlink():
                    info.create_system = 3
                    info.external_attr = 0o120777 << 16
                    archive.writestr(info, os.readlink(path))
                else:
                    archive.writestr(info, path.read_bytes())


def package(binary, target, version, output, identity=None, notary=None, desktop=False):
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
        if desktop:
            instructions = (
                'ModelCat ' + version + '\n'
                'Extract the app into a writable folder and open ModelCat.app.\n'
                'The first workspace startup downloads and verifies the Ubuntu image.\n'
                'The default workspace is stored in ModelCatVMData next to the app.\n'
                'If macOS restricts access to the workspace, choose its ModelCatVMData folder '
                'in the system folder picker under 存储位置 (Storage Location).\n'
                'Closing the main window or quitting ModelCat stops the workspace by default.\n'
                'Open 设置… (Settings) in the ModelCat menu to change this behavior.\n'
                'To stop the workspace manually, choose 停止工作区 (Stop Workspace) in the workspace menu.\n'
            )
        else:
            instructions = (
                'ModelCat ' + version + '\n'
                'Run ./modelcat start, then ./modelcat open.\n'
                'macOS: extract ModelCat.app and move it into Applications before opening.\n'
                'The first unsigned release requires macOS approval. No sudo or manual chmod is needed.\n'
                'Credentials are imported only after an explicit action in the local UI.\n'
            )
        (stage / 'README.txt').write_text(instructions + 'https://github.com/OpenDotAI/modelcat\n', encoding='utf-8')
        project = Path(__file__).resolve().parents[1]
        licenses = stage / 'licenses'
        licenses.mkdir()
        shutil.copyfile(project / 'LICENSE', licenses / 'ModelCat-LICENSE.txt')
        shutil.copyfile(project / 'vendor/asale/LICENSE', licenses / 'Asale-LICENSE.txt')
        shutil.copyfile(project / 'THIRD_PARTY_NOTICES.md', stage / 'THIRD_PARTY_NOTICES.md')
        files = []
        signing = 'not-applicable'
        if 'apple-darwin' in target:
            if sys.platform != 'darwin':
                raise ValueError('macOS bundles must be signed and packaged on macOS')
            app_name = 'ModelCat'
            app = stage / (app_name + '.app')
            macos = app / 'Contents/MacOS'
            macos.mkdir(parents=True)
            shutil.copyfile(executable, macos / 'modelcat')
            (macos / 'modelcat').chmod(0o755)
            resources = app / 'Contents/Resources'
            if desktop:
                shutil.copyfile(project / 'CLI.md', stage / 'CLI.md')
                runtime = project / '.vm-tools'
                # ModelCat uses the built-in VZ driver and invokes limactl directly.
                unused = shutil.ignore_patterns('lima-driver-krunkit', 'limactl-menu', 'limactl-mcp')
                shutil.copytree(runtime / 'lima', resources / 'vm-runtime/lima', symlinks=True, ignore=unused)
                shutil.copytree(runtime / 'guest', resources / 'vm-runtime/guest')
            shutil.copytree(licenses, resources / 'licenses')
            shutil.copyfile(stage / 'THIRD_PARTY_NOTICES.md', resources / 'THIRD_PARTY_NOTICES.md')
            shutil.copyfile(project / 'apps/seller/icons/ModelCat.icns', resources / 'ModelCat.icns')
            plist = {'CFBundleIdentifier': ('ai.opendot.modelcat.experiment' if desktop else 'ai.opendot.modelcat'), 'CFBundleName': app_name, 'CFBundleDisplayName': app_name, 'CFBundleExecutable': 'modelcat', 'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': version.split('-')[0].split('+')[0], 'CFBundleVersion': version.split('-')[0].split('+')[0], 'LSMinimumSystemVersion': ('13.5' if desktop else '12.0'), 'LSUIElement': not desktop, 'NSHighResolutionCapable': True}
            plist['CFBundleIconFile'] = 'ModelCat.icns'
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
            executable.unlink()  # The notarized app owns the one distributed Mach-O.
            app_zip = output / f'modelcat-{target}.zip'
            zip_tree(stage, app_zip)
            files.append(app_zip)
            dmg_root = stage / 'disk'
            dmg_root.mkdir()
            shutil.copytree(app, dmg_root / app.name, symlinks=True)
            if desktop:
                shutil.copyfile(stage / 'README.txt', dmg_root / 'README.txt')
                shutil.copyfile(stage / 'CLI.md', dmg_root / 'CLI.md')
            (dmg_root / 'Applications').symlink_to('/Applications')
            subprocess.run(['codesign', '--verify', '--strict', str(dmg_root / app.name)], check=True)
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
                tar.add(licenses, arcname='licenses')
                tar.add(stage / 'THIRD_PARTY_NOTICES.md', arcname='THIRD_PARTY_NOTICES.md')
            files.append(archive)
        metadata = {'version': version, 'target': target, 'signing': signing, 'files': [{'name': p.name, 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in files]}
        (output / f'modelcat-{target}.json').write_text(json.dumps(metadata, indent=2) + '\n')
        return metadata


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--desktop', action='store_true', help='Package the desktop-feature binary as the ModelCat app')
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--target', required=True)
    p.add_argument('--version', required=True)
    p.add_argument('--output', type=Path, default=Path('release'))
    p.add_argument('--sign-identity', default=os.environ.get('MODELCAT_SIGN_IDENTITY'))
    p.add_argument('--notary-profile', default=os.environ.get('MODELCAT_NOTARY_PROFILE'))
    args = p.parse_args()
    print(json.dumps(package(args.binary, args.target, args.version.removeprefix('v'), args.output, args.sign_identity, args.notary_profile, args.desktop), indent=2))
