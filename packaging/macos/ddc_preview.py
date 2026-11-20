"""Build the Apple Silicon DDC trial app with an ad hoc signature."""

import argparse
import json
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'build-support'))

from identity import IDENTITY
from localizations import DECLARATIONS, STRINGS, write_localizations
from macos_launch_smoke import launch_smoke
from payload import copy_notices
from provenance import digest, source_inventory, source_tree_sha256, verify
from versioning import macos_version_fields, package_build

TARGET = 'aarch64-apple-darwin'


def run(*args):
    subprocess.run([str(arg) for arg in args], cwd=ROOT, check=True)


def create_app_bundle(app, binary, bundle_id, version, build, *, backend=False):
    contents = app / 'Contents'
    mac = contents / 'MacOS'
    resources = contents / 'Resources'
    mac.mkdir(parents=True)
    resources.mkdir()
    shutil.copy2(binary, mac / binary.name)
    shutil.copy2(ROOT / 'apps/oink/src-tauri/icons/icon.icns', resources / 'icon.icns')
    info = dict(CFBundleExecutable=binary.name, CFBundleIdentifier=bundle_id,
                CFBundleName='Oink', CFBundleDisplayName='Oink', CFBundlePackageType='APPL',
                CFBundleIconFile='icon.icns', LSMinimumSystemVersion=IDENTITY['macos_minimum'],
                LSUIElement=True, NSHighResolutionCapable=True,
                **macos_version_fields(version, build), **DECLARATIONS)
    if backend:
        info.update(STRINGS['en'])
        info['NSBonjourServices'] = [IDENTITY['bonjour_service']]
    (contents / 'Info.plist').write_bytes(plistlib.dumps(info))
    write_localizations(app, permissions=backend)
    return mac, resources


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--staging', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--sequence', type=int, required=True)
    args = parser.parse_args()
    if platform.system() != 'Darwin' or platform.machine() != 'arm64':
        parser.error('an Apple Silicon macOS build host is required')
    staging, output = args.staging.resolve(), args.output.resolve()
    if any(path.is_relative_to(ROOT) for path in (staging, output)):
        parser.error('staging and output must be outside the source tree')
    staging.mkdir(parents=True, exist_ok=False)
    output.mkdir(parents=True, exist_ok=True)
    version = IDENTITY['version']
    build = package_build(args.sequence, False)
    inventory = source_inventory(ROOT)
    tree = source_tree_sha256(ROOT, inventory)
    snapshot = staging / 'source-inventory.json'
    snapshot.write_text(json.dumps({'files': inventory}), encoding='utf-8')
    run(sys.executable, ROOT / 'build-support/build.py', '--target', TARGET, '--build', build,
        '--source-tree-sha256', tree, '--source-inventory', snapshot)
    binaries = Path(os.environ.get('CARGO_TARGET_DIR', ROOT / 'target')) / TARGET / 'release'
    verify(binaries, tree, version, TARGET, build)

    names = IDENTITY['binaries']
    app = staging / 'Oink.app'
    mac, resources = create_app_bundle(app, binaries / names['backend'], IDENTITY['bundle_id'],
                                        version, build, backend=True)
    helper = app / 'Contents/Helpers/Oink.app'
    create_app_bundle(helper, binaries / names['ui'], IDENTITY['macos']['ui_id'], version, build)
    for role in ('agent', 'crash_collector'):
        shutil.copy2(binaries / names[role], mac / names[role])
    copy_notices(resources)
    shutil.copy2(ROOT / 'DDC.md', resources / 'DDC.md')
    (resources / IDENTITY['metadata_file']).write_text(json.dumps(dict(
        schema=1, product='Oink', variant='DDC preview', version=version, build=build,
        source_tree_sha256=tree, architecture='arm64', minimum_os=IDENTITY['macos_minimum'],
        signing='ad-hoc', notarized=False, update_policy='manual-only',
    ), indent=2) + '\n', encoding='utf-8')

    for binary in (*mac.iterdir(), helper / 'Contents/MacOS' / names['ui']):
        run('xcrun', 'lipo', '-verify_arch', 'arm64', binary)
        run('xcrun', 'strip', '-S', binary)
    for role, suffix in (('agent', '.agent'), ('crash_collector', '.crash-collector')):
        run('codesign', '--force', '--sign', '-', '--identifier', IDENTITY['bundle_id'] + suffix, mac / names[role])
    run('codesign', '--force', '--sign', '-', helper)
    run('codesign', '--force', '--sign', '-', app)
    run('codesign', '--verify', '--deep', '--strict', app)
    run('node', ROOT / 'build-support/check-package-licenses.mjs', app, 'macos-arm64')
    launch_smoke(mac / names['backend'])

    archive = output / f'Oink-DDC-{version}-macOS-arm64.zip'
    run('ditto', '-c', '-k', '--sequesterRsrc', '--keepParent', app, archive)
    shutil.copy2(ROOT / 'DDC.md', output / 'DDC.md')
    (output / 'SHA256SUMS').write_text(f'{digest(archive)}  {archive.name}\n', encoding='utf-8')


if __name__ == '__main__':
    main()
