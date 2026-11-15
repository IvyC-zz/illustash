"""只测资源暂存：用标记用的二进制和模型，不安装、不推理、不用编译器。"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from tests.windows_fixed_suite import skip_allowed

ROOT = Path(__file__).resolve().parents[1]


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'packaging' / f'{name}.py')
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


pack = module('build')


def desktop_required(name):
    """桌面宿主的必需组件清单（desktop/src/bundled_workers.rs）。"""
    source = (ROOT / 'desktop/src/bundled_workers.rs').read_text(encoding='utf-8')
    body = re.search(rf'const {name}: \[&str; \d+\] = \[(.*?)\];', source, re.S).group(1)
    return re.findall(r'"([^"]+)"', body)


def assert_desktop_contract(test, manifest, platform, name):
    """宿主只接受格式 1、research 变体、它自己的平台，以及清单里列出的组件。"""
    test.assertEqual((manifest['format'], manifest['variant'], manifest['platform']), (1, 'research', platform))
    required = desktop_required(name)
    test.assertTrue(required)
    test.assertEqual([path for path in required if path not in manifest['files']], [])
    assert_single_worker_and_notices(test, manifest, '.exe' if platform == 'win32' else '')


def assert_single_worker_and_notices(test, manifest, suffix, research=True):
    """#37：只有一个 worker 程序，没有重复的可执行文件；#8：附带本项目和第三方的声明。"""
    files = manifest['files']
    binaries = {name: digest for name, digest in files.items() if name.startswith('bin/')}
    test.assertEqual(len(set(binaries.values())), len(binaries), binaries)
    workers = [name for name in binaries if 'worker' in name or any(
        name == f'bin/illustash-{kind}{suffix}' for kind in pack.WORKER_KINDS)]
    test.assertEqual(workers, [f'bin/illustash-worker{suffix}'] if research else [])
    for installed, source in pack.NOTICES.items():
        test.assertEqual(files['resources/' + installed], hashlib.sha256((ROOT / source).read_bytes()).hexdigest())


class PackagingResourcesTest(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory(prefix='illustash-package-markers-')
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.binaries = self.root / 'inputs/bin'
        self.binaries.mkdir(parents=True)
        suffix = '.exe' if os.name == 'nt' else ''
        for name in ('illustash-terms', 'illustash-worker', 'illustash-iqdb', 'illustash-desktop'):
            (self.binaries / (name + suffix)).write_bytes(('marker:' + name).encode())
        self.iqdb = self.binaries / ('illustash-iqdb' + suffix)
        self.models = self.root / 'inputs/models'
        self.models.mkdir()
        for name in ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx',
                     'ccip/model_metrics.onnx', 'ccip/metrics.json'):
            target = self.models / name
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(('synthetic model marker:' + name).encode())
        (self.models / 'unlisted-private-marker').write_bytes(b'must not ship')
        (self.binaries / 'unlisted-private-marker').write_bytes(b'must not ship')

    def manifest(self, output):
        manifest = json.loads((output / 'manifest.json').read_text())
        files = {p.relative_to(output).as_posix(): hashlib.sha256(p.read_bytes()).hexdigest()
                 for p in output.rglob('*') if p.is_file() and p.name != 'manifest.json'}
        # 清单必须正好覆盖交付的全部文件，而不只是挑出来的几个名字。
        self.assertEqual(manifest['files'], files)
        self.assertTrue(all(not Path(name).is_absolute() and '..' not in Path(name).parts for name in files))
        self.assertFalse(any('unlisted-private-marker' in name for name in files))
        return manifest

    def test_base_full_difference_aliases_and_model_allowlist(self):
        base, full = self.root / 'base', self.root / 'full'
        pack.stage(base, self.binaries)
        pack.stage(full, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        plain, complete = self.manifest(base), self.manifest(full)
        self.assertEqual((plain['variant'], complete['variant']), ('base', 'research'))
        self.assertTrue(plain['files'].keys() <= complete['files'].keys())
        for name, digest in plain['files'].items():
            if name in ('start.sh', 'start.cmd'):
                self.assertNotEqual(complete['files'][name], digest)
            else:
                self.assertEqual(complete['files'][name], digest)
        delta = complete['files'].keys() - plain['files'].keys()
        self.assertEqual({name for name in delta if name.startswith('resources/')}, {
            'resources/models/wd_swinv2_v3_emb.onnx', 'resources/models/ccip/model_feat.onnx',
            'resources/models/ccip/model_metrics.onnx', 'resources/models/ccip/metrics.json',
            *(installed for installed, _ in pack.model_licenses())})
        suffix = '.exe' if os.name == 'nt' else ''
        self.assertEqual({name for name in delta if name.startswith('bin/')}, {
            f'bin/illustash-{name}{suffix}' for name in ('worker', 'iqdb')})
        worker = hashlib.sha256((self.binaries / ('illustash-worker' + suffix)).read_bytes()).hexdigest()
        self.assertEqual(complete['files'][f'bin/illustash-worker{suffix}'], worker)
        assert_single_worker_and_notices(self, plain, suffix, research=False)
        assert_single_worker_and_notices(self, complete, suffix)
        self.assertFalse((base / 'resources/licenses/models').exists())
        # 安装后的每个请求都是原生的：不附带 Python 计算程序。
        self.assertFalse(any(name.startswith(('workers/', 'runtime/')) for name in delta))
        self.assertFalse((base / 'workers').exists())
        self.assertFalse((base / 'runtime').exists())
        self.assertFalse((base / 'resources/models').exists())

    def test_static_notices_preserve_fixed_dependency_bytes(self):
        for relative, digest in {
            'native/terms/vendor/pocketfft/LICENSE.md': 'a85ca13fdf90160b64a0698215868c13b74d835ad0a4e2ba44713b8c5058a056',
            'native/terms/licenses/LCMS.txt': '9ed584604d11ed0ab26211e1775687283008d35e164baa8294757fbd4d829f90',
        }.items():
            self.assertEqual(hashlib.sha256((ROOT / relative).read_bytes()).hexdigest(), digest)

    def test_relocation_hashes_zip_paths_and_exclusive_output(self):
        original = self.root / 'artifact with spaces'
        pack.stage(original, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        before = self.manifest(original)
        relocated = self.root / '新位置' / original.name
        relocated.parent.mkdir()
        original.rename(relocated)
        self.assertEqual(self.manifest(relocated), before)
        first, second = self.root / 'one.zip', self.root / 'two.zip'
        pack.archive(relocated, first)
        pack.archive(relocated, second)
        self.assertEqual(first.read_bytes(), second.read_bytes())
        with zipfile.ZipFile(first) as archive:
            self.assertEqual(set(archive.namelist()), {
                f'{relocated.name}/{name}' for name in before['files']} | {f'{relocated.name}/manifest.json'})
            for item in archive.infolist():
                self.assertEqual(item.date_time, (1980, 1, 1, 0, 0, 0))
        with self.assertRaisesRegex(ValueError, 'already exists'):
            pack.stage(relocated, self.binaries)
        with self.assertRaisesRegex(ValueError, 'already exists'):
            pack.archive(relocated, first)
        self.assertEqual(self.manifest(relocated), before)

    def test_missing_model_is_reported_and_symlink_resource_rejected(self):
        (self.models / 'ccip/model_feat.onnx').unlink()
        with self.assertRaisesRegex(FileNotFoundError, 'model_feat.onnx'):
            pack.stage(self.root / 'incomplete', self.binaries, research=True,
                       iqdb=self.iqdb, models=self.models)
        self.assertFalse((self.root / 'incomplete/manifest.json').exists())
        if os.name != 'nt':
            source = self.root / 'link'
            source.symlink_to(self.iqdb)
            with self.assertRaisesRegex(ValueError, 'symlink'):
                pack.copy(source, self.root / 'copied')
            self.assertFalse((self.root / 'copied').exists())

    def test_explicit_cpu_runtime_and_notices_are_manifested(self):
        runtime = self.root / 'existing-cpu-runtime'
        runtime.write_bytes(b'synthetic CPU runtime marker')
        notices = self.root / 'notices'
        notices.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (notices / name).write_bytes(('synthetic notice:' + name).encode())
        (notices / 'unlisted-private-marker').write_bytes(b'not allowed')
        # 只测跨平台的源暂存，不检查安装后的 macOS 运行时。
        with patch.object(pack.sys, 'platform', 'darwin'):
            if os.name == 'nt':
                for name in ('illustash-terms', 'illustash-worker'):
                    (self.binaries / name).write_bytes(b'synthetic marker:' + name.encode())
            output = self.root / 'explicit-native-resources'
            with patch.object(pack.subprocess, 'run') as compiler:
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb,
                           ort_dylib=runtime, ort_licenses=notices)
                compiler.assert_not_called()
            manifest = self.manifest(output)
            for name, source in {
                'resources/onnxruntime/libonnxruntime.dylib': runtime,
                'resources/licenses/onnxruntime/LICENSE': notices / 'LICENSE',
                'resources/licenses/onnxruntime/ThirdPartyNotices.txt': notices / 'ThirdPartyNotices.txt',
                'resources/licenses/pocketfft/LICENSE.md': ROOT / 'native/terms/vendor/pocketfft/LICENSE.md',
                'resources/licenses/LCMS.txt': ROOT / 'native/terms/licenses/LCMS.txt',
            }.items():
                self.assertEqual(manifest['files'][name], hashlib.sha256(source.read_bytes()).hexdigest())
        for values in ({'ort_dylib': runtime}, {'ort_licenses': notices}):
            with self.assertRaisesRegex(ValueError, 'ORT runtime'):
                pack.stage(self.root / 'unpaired', self.binaries, research=True, **values)
            self.assertFalse((self.root / 'unpaired').exists())
        with self.assertRaisesRegex(ValueError, '--research'):
            pack.stage(self.root / 'wrong-variant', self.binaries, ort_dylib=runtime, ort_licenses=notices)
        self.assertFalse((self.root / 'wrong-variant').exists())
        # #271：DirectML.dll 只随 Windows 运行时一起打包，而且它的包必须完整。
        directml = self.root / 'directml'
        for source in list(pack.DIRECTML_FILES.values())[1:]:
            (directml / source).parent.mkdir(parents=True, exist_ok=True)
            (directml / source).write_bytes(b'synthetic')
        with patch.object(pack.sys, 'platform', 'darwin'), self.assertRaisesRegex(ValueError, 'Windows ORT'):
            pack.stage(self.root / 'directml-mac', self.binaries, research=True, ort_dylib=runtime,
                       ort_licenses=notices, directml=directml)
        with patch.object(pack.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'Windows ORT'):
            pack.stage(self.root / 'directml-alone', self.binaries, research=True, directml=directml)
        with patch.object(pack.sys, 'platform', 'win32'), self.assertRaisesRegex(FileNotFoundError, 'DirectML.dll'):
            pack.stage(self.root / 'directml-incomplete', self.binaries, research=True, ort_dylib=runtime,
                       ort_licenses=notices, directml=directml)
        for name in ('directml-mac', 'directml-alone', 'directml-incomplete'):
            self.assertFalse((self.root / name).exists())
        # #381：Windows 上不打包登记在微软遥测组里的运行库；全零组（自建的那份）照常通过。合成的 TraceLogging
        # provider 元数据：总长、名字、组 trait（长度 0x13、类型 1、组 GUID）。
        def provider(group):
            name = b'Microsoft.ML.ONNXRuntime\0'
            trait = b'\x13\x00\x01' + group
            return len(name + trait).to_bytes(2, 'little') + name + trait
        official = self.root / 'official-onnxruntime.dll'
        official.write_bytes(b'MZ synthetic' + provider(pack.MICROSOFT_TELEMETRY_GROUP) + b'tail')
        with patch.object(pack.sys, 'platform', 'win32'), self.assertRaisesRegex(ValueError, 'telemetry group'):
            pack.stage(self.root / 'official-runtime', self.binaries, research=True, ort_dylib=official,
                       ort_licenses=notices)
        self.assertFalse((self.root / 'official-runtime').exists())
        built = self.root / 'built-onnxruntime.dll'
        built.write_bytes(b'MZ synthetic' + provider(bytes(16)) + b'tail')
        pack.check_windows_ort_telemetry(built)
        (notices / 'ThirdPartyNotices.txt').unlink()
        with self.assertRaisesRegex(FileNotFoundError, 'ThirdPartyNotices'):
            pack.stage(self.root / 'incomplete-notices', self.binaries, research=True,
                       iqdb=self.iqdb, ort_dylib=runtime, ort_licenses=notices)
        self.assertFalse((self.root / 'incomplete-notices/manifest.json').exists())

    def test_model_licenses_ship_verbatim_with_every_computation_package(self):
        """#8：计算包附带已提交的许可原文，不论是否带模型（#168：不带时由 App 下载模型）；
        除此之外没有别的来源。"""
        registry = json.loads((pack.MODEL_LICENSES / 'sources.json').read_text())['files']
        self.assertEqual(sorted(registry), ['ccip/BigScience-OpenRAIL-M.pdf', 'ccip/deepghs-ccip-README.md',
                                            'ccip/deepghs-ccip_onnx-README.md', 'wd-swinv2-tagger-v3/LICENSE-2.0.txt',
                                            'wd-swinv2-tagger-v3/SmilingWolf-wd-swinv2-tagger-v3-README.md',
                                            'wd-swinv2-tagger-v3/deepghs-wd14_tagger_with_embeddings-README.md'])
        notice = (pack.MODEL_LICENSES / 'NOTICE.txt').read_text()
        # 声明列出每个附带的模型文件和适用于它的许可文本。
        for name in ('wd_swinv2_v3_emb.onnx', 'model_feat.onnx', 'model_metrics.onnx', 'metrics.json',
                     'wd-swinv2-tagger-v3/LICENSE-2.0.txt', 'ccip/BigScience-OpenRAIL-M.pdf'):
            self.assertIn(name, notice)
        output = self.root / 'with models'
        pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
        manifest = self.manifest(output)
        shipped = sorted(name for name in manifest['files'] if name.startswith('resources/licenses/models/'))
        self.assertEqual(shipped, sorted(f'resources/licenses/models/{name}'
                                         for name in ('NOTICE.txt', 'sources.json', *registry)))
        for name in shipped:
            source = pack.MODEL_LICENSES / name.removeprefix('resources/licenses/models/')
            self.assertEqual((output / name).read_bytes(), source.read_bytes())
        without = self.root / 'without models'
        pack.stage(without, self.binaries, research=True, iqdb=self.iqdb)
        self.assertEqual(sorted(name for name in self.manifest(without)['files']
                                if name.startswith('resources/licenses/models/')), shipped)
        self.assertFalse((without / 'resources/models').exists())
        self.assertEqual(pack.MODEL_FILES, ('wd_swinv2_v3_emb.onnx', 'ccip/model_feat.onnx',
                                            'ccip/model_metrics.onnx', 'ccip/metrics.json'))

    def test_wd_variant_is_optional_and_checked_before_staging(self):
        output = self.root / 'optional variant'
        entry = pack.MODEL_MANIFEST['wd_coreml']['file']
        pack.stage_wd_coreml(self.models, output)
        self.assertFalse(output.exists())
        variant = self.models / entry['path']
        variant.parent.mkdir(parents=True)
        variant.write_bytes(b'damaged')
        with self.assertRaisesRegex(ValueError, 'does not match'):
            pack.stage_wd_coreml(self.models, output)
        # 用小合成文件覆盖同一条校验和复制路径；不把模型权重塞进测试。
        synthetic = {'file': dict(entry, size=7, sha256=hashlib.sha256(b'valid!!').hexdigest())}
        with patch.dict(pack.MODEL_MANIFEST, wd_coreml=synthetic):
            variant.write_bytes(b'valid!!')
            pack.stage_wd_coreml(self.models, output)
            self.assertEqual((output / entry['path']).read_bytes(), b'valid!!')

    def test_packages_with_models_refuse_incomplete_model_licenses(self):
        def broken(name, change):
            source = self.root / name
            shutil.copytree(pack.MODEL_LICENSES, source)
            change(source)
            return source
        cases = {
            'missing': (lambda s: (s / 'ccip/BigScience-OpenRAIL-M.pdf').unlink(), 'missing: ccip/BigScience'),
            'altered': (lambda s: (s / 'wd-swinv2-tagger-v3/LICENSE-2.0.txt').write_bytes(b'edited'), 'differs'),
            'no notice': (lambda s: (s / 'NOTICE.txt').unlink(), "missing \\['NOTICE.txt'\\]"),
            'unregistered': (lambda s: (s / 'ccip/extra.txt').write_bytes(b'x'), 'unregistered'),
            'no registry': (lambda s: (s / 'sources.json').unlink(), 'registry missing'),
        }
        for name, (change, message) in cases.items():
            with self.subTest(name), patch.object(pack, 'MODEL_LICENSES', broken(name, change)):
                output = self.root / f'out {name}'
                with self.assertRaisesRegex(ValueError, message):
                    pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models)
                self.assertFalse(output.exists())

    def test_rust_licenses_cover_every_locked_crate_and_ship(self):
        """#161：只要 Cargo.lock 改了而没重新运行 tools/gen_rust_licenses.py，就失败。"""
        text = pack.rust_licenses()
        self.assertEqual(pack.NOTICES['licenses/rust-dependencies.txt'], text.relative_to(ROOT).as_posix())
        registry = json.loads((pack.RUST_LICENSES / 'crates.json').read_text())
        self.assertEqual(registry['generator'], 'cargo-about 0.9.2')
        # 附带的索引正好列出有许可文本的 crate，每个至少带一个文本编号。
        body = text.read_text(encoding='utf-8')
        listed = re.findall(r'^(\S+ \S+) \(.+\): \[\d+\]', body.split('\nLicense texts\n')[0], re.M)
        self.assertEqual(sorted(listed), sorted({name for entry in registry['workspaces'].values()
                                                 for name in entry['crates']}))
        self.assertIn('tiny_http 0.12.0', listed)  # vendored 的路径依赖
        self.assertFalse(any(name.startswith('illustash-') for name in listed))
        for packaged in ('base', 'research'):
            output = self.root / packaged
            pack.stage(output, self.binaries, research=packaged == 'research', iqdb=self.iqdb)
            self.assertEqual(self.manifest(output)['files']['resources/licenses/rust-dependencies.txt'],
                             hashlib.sha256(text.read_bytes()).hexdigest())
        pack.resources(self.root / 'desktop-resources')
        self.assertEqual((self.root / 'desktop-resources/licenses/rust-dependencies.txt').read_bytes(), text.read_bytes())

    def test_lock_file_parsing_keeps_vendored_and_drops_own_crates(self):
        lock = self.root / 'Cargo.lock'
        lock.write_text('# generated\nversion = 4\n\n[[package]]\nname = "illustash-core"\nversion = "0.1.0"\n'
                        'dependencies = [\n "serde",\n]\n\n[[package]]\nname = "serde"\nversion = "1.0.1"\n'
                        'source = "registry+https://github.com/rust-lang/crates.io-index"\nchecksum = "00"\n\n'
                        '[[package]]\nname = "tiny_http"\nversion = "0.12.0"\n\n'
                        '[[patch.unused]]\nname = "unused"\nversion = "9.9.9"\n'.replace('\n', '\r\n'))
        self.assertEqual(pack.third_party_crates(lock), {'serde 1.0.1', 'tiny_http 0.12.0'})

    def test_stale_rust_licenses_refuse_packaging(self):
        def lock_root(name, workspace, change):
            root = self.root / name
            for each in pack.RUST_WORKSPACES:
                (root / each).mkdir(parents=True)
                shutil.copy2(ROOT / each / 'Cargo.lock', root / each / 'Cargo.lock')
            lock = root / workspace / 'Cargo.lock'
            lock.write_text(change(lock.read_text()))
            return root
        added = '\n[[package]]\nname = "zzz-new"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n'
        cases = {
            'added': ('desktop', lambda text: text + added, r"desktop/Cargo.lock: unlisted \['zzz-new 1.0.0'\]"),
            'bumped': ('crates/iqdb', lambda text: text.replace('name = "ring"\nversion = "0.17.14"', 'name = "ring"\nversion = "0.17.15"'),
                       r"unlisted \['ring 0.17.15'\], no longer locked \['ring 0.17.14'\]"),
            'removed': ('native/terms', lambda text: text.replace('name = "zlib-rs"', 'name = "illustash-zlib-rs"'),
                        r"no longer locked \['zlib-rs 0.6.8'\]"),
        }
        for name, (workspace, change, message) in cases.items():
            with self.subTest(name), self.assertRaisesRegex(ValueError, message + '.*gen_rust_licenses'):
                pack.rust_licenses(root=lock_root(name, workspace, change))
        def broken(name, change):
            source = self.root / name
            shutil.copytree(pack.RUST_LICENSES, source)
            change(source)
            return source
        for name, (change, message) in {
            'edited text': (lambda s: (s / 'rust-dependencies.txt').write_bytes(b'edited'), 'differ from crates.json'),
            'no text': (lambda s: (s / 'rust-dependencies.txt').unlink(), 'texts missing'),
        }.items():
            with self.subTest(name), patch.object(pack, 'RUST_LICENSES', broken(name, change)):
                output = self.root / f'out {name}'
                with self.assertRaisesRegex(ValueError, message):
                    pack.stage(output, self.binaries)
                self.assertFalse(output.exists())

    def test_full_installers_ship_resources_once(self):
        """#37：完整安装包只用 compute/resources；基础安装包保留 resources/。"""
        def mapping(name):
            return json.loads((ROOT / 'packaging' / name).read_text())['bundle']['resources']
        self.assertEqual(mapping('tauri-bundle.json'), {'../packaging/desktop-resources/': 'resources/'})
        for name in ('tauri-full.json', 'tauri-full-macos.json'):
            # 宿主以 compute/resources 作为完整安装的根（见 bundled_workers 的测试），所以顶层再放一份
            # resources/ 不会有人用。
            self.assertEqual(mapping(name), {'../packaging/desktop-compute/': 'compute/'}, name)
        # 升级旧的完整安装时，不能留下按类型分开的副本或 resources/。
        config = json.loads((ROOT / 'packaging/tauri-full.json').read_text())
        hooks = (ROOT / 'desktop' / config['bundle']['windows']['nsis']['installerHooks']).resolve()
        text = hooks.read_text()
        self.assertIn('!macro NSIS_HOOK_PREINSTALL', text)
        for kind in pack.WORKER_KINDS:
            self.assertIn(f'Delete "$INSTDIR\\compute\\bin\\illustash-{kind}.exe"', text)
        self.assertIn('RMDir /r "$INSTDIR\\resources"', text)
        self.assertNotIn('worker', ''.join(line for line in text.splitlines() if line.lstrip().startswith(('Delete', 'RMDir'))))

    def test_windows_installers_follow_the_display_language(self):
        """#173：两个 NSIS 安装包都有英文和简体中文页面，由 NSIS 按 Windows 显示语言选择，不弹对话框，
        并把选择记下来供卸载程序使用。"""
        shared = ROOT / 'packaging/nsis-hooks.nsh'
        save = 'WriteRegStr HKCU "${MANUPRODUCTKEY}" "Installer Language" $LANGUAGE'
        self.assertIn('!macro NSIS_HOOK_POSTINSTALL', shared.read_text())
        self.assertIn(save, shared.read_text())
        for name in ('tauri-bundle.json', 'tauri-full.json'):
            nsis = json.loads((ROOT / 'packaging' / name).read_text())['bundle']['windows']['nsis']
            # 英文放第一：两种都不匹配时，NSIS 退回第一种语言。
            self.assertEqual(nsis['languages'], ['English', 'SimpChinese'], name)
            self.assertNotIn('displayLanguageSelector', nsis, name)
            hooks = (ROOT / 'desktop' / nsis['installerHooks']).resolve()
            text = hooks.read_text()
            self.assertTrue(hooks == shared or '!include "${__FILEDIR__}\\nsis-hooks.nsh"' in text, name)
            self.assertEqual(text.count('NSIS_HOOK_POSTINSTALL'), 0 if hooks != shared else 1, name)

    @unittest.skipIf(os.name == 'nt', 'POSIX shell launcher execution only')
    def test_relocated_ordinary_launch_preserves_overrides_and_remains_lazy(self):
        keys = ('ILLUSTASH_ROOT', 'ILLUSTASH_MODELS', 'ILLUSTASH_SHARED',
                'ILLUSTASH_RESEARCH_WORKER', 'ILLUSTASH_INDEX_WORKER',
                'ILLUSTASH_PROVENANCE_WORKER', 'ILLUSTASH_RECOGNITION_WORKER')
        probe = '#!/bin/sh\n' + ''.join(f'printf "%s\\n" "${key}"\n' for key in keys) + 'printf "%s\\n" "$@"\n'
        for name in ('illustash-terms', 'illustash-desktop'):
            binary = self.binaries / name
            binary.write_text(probe)
            binary.chmod(0o755)
        worker = self.binaries / 'illustash-worker'
        worker.write_text('#!/bin/sh\n/bin/touch "$HOME/worker-was-started"\nexit 97\n')
        worker.chmod(0o755)
        output = self.root / 'before move'
        pack.stage(output, self.binaries, research=True, iqdb=self.iqdb,
                   desktop=self.binaries / 'illustash-desktop')
        moved = self.root / '已移动 with spaces'
        output.rename(moved)
        home, cwd = self.root / 'synthetic home', self.root / 'unrelated cwd'
        home.mkdir(); cwd.mkdir()
        env = {'HOME': str(home), 'PATH': ''}
        shared = (home / 'Library/Application Support/Illustash/Shared' if pack.sys.platform == 'darwin'
                  else home / '.local/share/Illustash/Shared')
        expected = [str(moved / 'resources'), str(moved / 'resources/models'), str(shared)] + [
            str(moved / 'bin/illustash-worker')] * 4
        for script, args in (('start.sh', ['http', '--native']), ('desktop.sh', [])):
            with self.subTest(script=script):
                result = subprocess.run(['/bin/sh', str(moved / script), '--library', 'synthetic.pilib'],
                                        cwd=cwd, env=env, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), expected + args + [
                    '--root', str(moved / 'resources'), '--library', 'synthetic.pilib'])
                overrides = {key: str(self.root / ('override ' + key)) for key in keys}
                result = subprocess.run(['/bin/sh', str(moved / script)], cwd=cwd,
                                        env={**env, **overrides}, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.splitlines(), list(overrides.values()) + args + [
                    '--root', overrides['ILLUSTASH_ROOT']])
        self.assertFalse((home / 'worker-was-started').exists())
        self.assertFalse(shared.exists())
        self.assertFalse((cwd / 'review').exists())


class CandidateExampleWiringTest(unittest.TestCase):
    def test_full_runners_build_and_export_each_current_candidate_before_discovery(self):
        shell = (ROOT / 'tools/check_all.sh').read_text(encoding='utf-8')
        ci = (ROOT / '.github/workflows/ci.yml').read_text(encoding='utf-8')
        windows = ci.split('\n  windows:\n', 1)[1].split('\n  ok:\n', 1)[0]
        probes = {'READ_HTTP': 'read-http', 'RECOGNITION_COMPUTE': 'recognition-compute',
                  'INDEX_COMPUTE': 'index-compute', 'RESEARCH_COMPUTE': 'research-compute',
                  'RELATION_PROBE': 'relation-probe', 'PROVENANCE_COMPUTE': 'provenance-compute'}
        for key, example in probes.items():
            for source, suffix, runner in ((shell, '', 'tests.run_fixed_suite -v -b'),
                                           (ci, '', 'tests.run_fixed_suite -v -b'),
                                           (windows, '.exe', 'tests.windows_fixed_suite')):
                build_at = source.index('--example ' + example)
                export_at = source.index('ILLUSTASH_' + key + '_BIN')
                discover_at = source.index('-m ' + runner)
                self.assertLess(build_at, export_at)
                self.assertLess(export_at, discover_at)
                self.assertIn('native/terms/target/release/examples/' + example + suffix, source)
        # #4：只对目标为 main 的同库 PR 自动运行，不用带写权限和 secret 的 pull_request_target。
        self.assertIn('\n  pull_request:\n    branches: [main]\n', ci)
        self.assertNotIn('pull_request_target', ci)
        self.assertIn('github.event.pull_request.head.repo.full_name == github.repository', ci)
        self.assertIn('!github.event.repository.private', ci)
        self.assertIn('\npermissions:\n  contents: read\n', ci)
        self.assertNotIn('secrets.', ci)


    def test_full_runners_accept_research_platform_skips_but_not_missing_candidates_or_models(self):
        import re
        patterns = []
        for name in ('tools/check_all.sh', '.github/workflows/ci.yml'):
            source = (ROOT / name).read_text(encoding='utf-8')
            pattern = re.search(r"allowed=['\"]([^'\"]+)['\"]", source).group(1)
            patterns.append(pattern)
            for reason in ('Windows pinned OpenBLAS backend only',
                           'explicit Windows native development calculator only',
                           'explicit Windows development App publication only',
                           'Linux is frozen: no local computation'):
                self.assertRegex(reason, pattern)
            for reason in ('explicit candidate research calculator/core required',
                           'explicit candidate host and its built worker required',
                           'explicit candidate math example and matching reference DLL required',
                           'explicit authorised CCIP model/runtime required'):
                self.assertNotRegex(reason, pattern)
        self.assertEqual(*patterns)

    def test_windows_runner_checks_whole_skip_reason(self):
        for reason in ('固定期望录自 POSIX，包名含 Windows 不允许的 ?',
                       'Apple 照片图库只在 macOS 上提供',
                       'Windows 上原生 worker 按设计拒绝启动（app_tasks），不适用'):
            self.assertTrue(skip_allowed(f'synthetic.case ... skipped {reason!r}'), reason)
            self.assertFalse(skip_allowed(f'synthetic.case ... skipped {reason!r} extra diagnostic'), reason)
        for reason in ('explicit candidate math example and matching reference DLL required',
                       'explicit authorised CCIP model/runtime required',
                       'Windows DPAPI unavailable on POSIX',
                       'POSIX: missing candidate',
                       'not enough free space for the synthetic archive',
                       'set ILLUSTASH_TERMS_BIN explicitly; absence is not upgrade acceptance'):
            self.assertFalse(skip_allowed(f'synthetic.case ... skipped {reason!r}'), reason)


class MacDesktopResourcesTest(unittest.TestCase):
    setUp = PackagingResourcesTest.setUp
    manifest = PackagingResourcesTest.manifest

    def test_complete_app_layout_relocation_and_incomplete_input_refusal(self):
        runtime = self.root / 'runtime-marker'
        runtime.write_bytes(b'synthetic runtime')
        notices = self.root / 'notices'
        notices.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (notices / name).write_text('synthetic notice')
        with patch.object(pack.sys, 'platform', 'darwin'):
            if os.name == 'nt':
                for name in ('illustash-terms', 'illustash-worker'):
                    (self.binaries / name).write_bytes(b'synthetic marker:' + name.encode())
            source = self.root / 'portable'
            pack.stage(source, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       desktop=self.binaries / ('illustash-desktop.exe' if os.name == 'nt' else 'illustash-desktop'),
                       ort_dylib=runtime, ort_licenses=notices)
            (source / 'private').mkdir()
            (source / 'private/library.sqlite').write_text('not a shipped resource')
            output = self.root / 'App.app/Contents/Resources/compute'
            pack.macos_desktop_compute(source, output)
            installed = self.manifest(output)
            original = json.loads((source / 'manifest.json').read_text())
            self.assertEqual((installed['variant'], installed['platform']), ('research', 'darwin'))
            assert_desktop_contract(self, installed, 'darwin', 'MACOS_FILES')
            self.assertEqual(installed['files'], {name: digest for name, digest in original['files'].items()
                             if name.startswith(('bin/', 'resources/', 'dependencies/'))
                             and Path(name).name != 'illustash-desktop'})
            self.assertFalse((output / 'bin/illustash-desktop').exists())
            self.assertFalse((output / 'private').exists())
            self.assertFalse((output / 'start.sh').exists())
            moved = self.root / 'relocated app with spaces'
            (self.root / 'App.app').rename(moved)
            self.assertEqual(self.manifest(moved / 'Contents/Resources/compute'), installed)
            with self.assertRaisesRegex(ValueError, 'already exists'):
                pack.macos_desktop_compute(source, moved / 'Contents/Resources/compute')
            lost = source / 'resources/web/static/app.js'
            saved = lost.read_bytes()
            lost.unlink()
            with self.assertRaisesRegex(ValueError, 'source manifest file set'):
                pack.macos_desktop_compute(source, self.root / 'lost-resource')
            self.assertFalse((self.root / 'lost-resource').exists())
            lost.write_bytes(saved)
            # #161：Rust crate 的许可文本也是必需组件。
            rust = source / 'resources/licenses/rust-dependencies.txt'
            saved = rust.read_bytes()
            rust.unlink()
            with self.assertRaisesRegex(ValueError, 'missing or unmanifested: resources/licenses/rust-dependencies.txt'):
                pack.macos_desktop_compute(source, self.root / 'no rust licenses')
            self.assertFalse((self.root / 'no rust licenses').exists())
            rust.write_bytes(saved)
            runtime = source / 'resources/onnxruntime/libonnxruntime.dylib'
            runtime.write_bytes(b'changed after staging')
            with self.assertRaisesRegex(ValueError, 'does not match manifest'):
                pack.macos_desktop_compute(source, self.root / 'bad-hash')
            self.assertFalse((self.root / 'bad-hash').exists())
            runtime.unlink()
            with self.assertRaisesRegex(ValueError, 'component missing'):
                pack.macos_desktop_compute(source, self.root / 'incomplete')
            self.assertFalse((self.root / 'incomplete').exists())


    def test_release_app_has_no_models_and_a_partial_set_is_refused(self):
        """#168：不加 --models 时 App 不带任何模型文件（由 App 下载）；绝不只带一部分。"""
        runtime = self.root / 'runtime-marker'
        runtime.write_bytes(b'synthetic runtime')
        notices = self.root / 'notices'
        notices.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (notices / name).write_text('synthetic notice')
        with patch.object(pack.sys, 'platform', 'darwin'):
            if os.name == 'nt':
                for name in ('illustash-terms', 'illustash-worker'):
                    (self.binaries / name).write_bytes(b'synthetic marker:' + name.encode())
            source = self.root / 'portable'
            pack.stage(source, self.binaries, research=True, iqdb=self.iqdb, ort_dylib=runtime, ort_licenses=notices)
            output = self.root / 'Release.app/Contents/Resources/compute'
            pack.macos_desktop_compute(source, output)
            installed = self.manifest(output)
            assert_desktop_contract(self, installed, 'darwin', 'MACOS_FILES')
            self.assertFalse(any(name.startswith('resources/models/') for name in installed['files']))
            self.assertTrue(any(name.startswith('resources/licenses/models/') for name in installed['files']))
            # 只列出部分模型的暂存布局，两种包都不是。
            manifest = json.loads((source / 'manifest.json').read_text())
            model = source / 'resources/models/wd_swinv2_v3_emb.onnx'
            model.parent.mkdir(parents=True)
            model.write_bytes(b'synthetic')
            manifest['files']['resources/models/wd_swinv2_v3_emb.onnx'] = hashlib.sha256(b'synthetic').hexdigest()
            (source / 'manifest.json').write_text(json.dumps(manifest))
            with self.assertRaisesRegex(ValueError, 'all present or all absent; missing resources/models/ccip/model_feat'):
                pack.macos_desktop_compute(source, self.root / 'partial')
            self.assertFalse((self.root / 'partial').exists())


class WindowsOpenBLASResourcesTest(unittest.TestCase):
    """只在合成身份上暂存源文件，从不做 Windows 加载或 ABI 测试。"""
    setUp = PackagingResourcesTest.setUp
    manifest = PackagingResourcesTest.manifest
    def make_runtime(self):
        """一个显式的 site-packages，里面放固定的 NumPy wheel，外加一个 CPU ORT 标记文件。"""
        site = self.root / 'explicit wheel/site-packages'
        libs = site / 'numpy.libs'
        libs.mkdir(parents=True)
        self.ort = self.root / 'ort/onnxruntime.dll'
        self.ort.parent.mkdir()
        self.ort.write_bytes(b'synthetic CPU runtime marker')
        self.ort_licenses = self.root / 'ort-licenses'
        self.ort_licenses.mkdir()
        for name in ('LICENSE', 'ThirdPartyNotices.txt'):
            (self.ort_licenses / name).write_bytes(b'synthetic ORT notice')
        # 合成的 Microsoft.AI.DirectML 包目录（#271）。
        self.directml = self.root / 'Microsoft.AI.DirectML.synthetic'
        for source in pack.DIRECTML_FILES.values():
            (self.directml / source).parent.mkdir(parents=True, exist_ok=True)
            (self.directml / source).write_bytes(b'synthetic DirectML package file:' + source.encode())
        self.dll = libs / 'fixed.dll'
        self.dll.write_bytes(b'synthetic registered DLL, never executable')
        self.digest = hashlib.sha256(self.dll.read_bytes()).hexdigest()
        self.notices = site / 'numpy-synthetic.dist-info/licenses'
        self.notices.mkdir(parents=True)
        (self.notices / 'LICENSE.txt').write_bytes(b'synthetic full NumPy/OpenBLAS bundled notice')
        (self.notices / 'nested').mkdir()
        (self.notices / 'nested/third-party.txt').write_bytes(b'synthetic transitive dependency notice')
        registry = patch.dict(pack.RESEARCH_OPENBLAS_IDENTITIES,
                              {self.digest: dict(version='synthetic', numpy='synthetic')}, clear=True)
        registry.start()
        self.addCleanup(registry.stop)
        # 标记用的二进制没法用 dumpbin 检查；真实的 CRT 导入由 WindowsCRTTest 和安装后的 Windows 验收另行覆盖。
        crt = patch.object(pack, 'check_windows_crt')
        crt.start()
        self.addCleanup(crt.stop)
        # 只有标记用的二进制取 Windows 文件名。这是跨平台的源测试。
        for name in ('illustash-terms', 'illustash-worker'):
            (self.binaries / (name + '.exe')).write_bytes(b'synthetic marker:' + name.encode())
        return site

    def test_fixed_resource_and_complete_notices_manifest_relocation(self):
        runtime = self.make_runtime()
        output = self.root / 'portable'
        with patch.object(pack.sys, 'platform', 'win32'):
            pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       openblas_site=runtime, ort_dylib=self.ort, ort_licenses=self.ort_licenses,
                       directml=self.directml)
            manifest = self.manifest(output)
            self.assertEqual(manifest['files']['resources/openblas/openblas.dll'], self.digest)
            for installed, source in pack.DIRECTML_FILES.items():
                self.assertEqual(manifest['files'][installed],
                                 hashlib.sha256((self.directml / source).read_bytes()).hexdigest())
            self.assertEqual(manifest['files']['resources/onnxruntime/backends.json'],
                             hashlib.sha256(pack.WINDOWS_ORT_BACKENDS.read_bytes()).hexdigest())
            self.assertFalse(any(name.startswith(('workers/', 'runtime/')) for name in manifest['files']))
            self.assertNotIn('ILLUSTASH_WORKER_PYTHON', (output / 'start.cmd').read_text())
            for source in self.notices.rglob('*'):
                if source.is_file():
                    target = output / 'resources/licenses/openblas' / source.relative_to(self.notices)
                    self.assertEqual(target.read_bytes(), source.read_bytes())
            compute = self.root / 'relocated with spaces/compute'
            pack.desktop_compute(output, compute)
            assert_desktop_contract(self, self.manifest(compute), 'win32', 'WINDOWS_FILES')
            self.assertEqual((compute / 'resources/openblas/openblas.dll').read_bytes(), self.dll.read_bytes())
            self.assertIn('ILLUSTASH_RESEARCH_OPENBLAS_DLL=%ILLUSTASH_ROOT%\\openblas\\openblas.dll', (output / 'start.cmd').read_text())
            self.assertIn('if not defined ILLUSTASH_RESEARCH_OPENBLAS_DLL', (output / 'research.cmd').read_text())
            (output / 'resources/openblas/openblas.dll').write_bytes(b'changed after staging')
            with self.assertRaisesRegex(ValueError, 'fixed reference identity'):
                pack.desktop_compute(output, self.root / 'bad compute')
            self.assertFalse((self.root / 'bad compute').exists())

    def test_unregistered_missing_ambiguous_and_symlink_resources_fail_before_staging(self):
        runtime = self.make_runtime()
        output = self.root / 'bad'
        def stage():
            with patch.object(pack.sys, 'platform', 'win32'):
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, openblas_site=runtime)
        self.dll.write_bytes(b'unregistered')
        with self.assertRaisesRegex(ValueError, 'exactly one fixed'):
            stage()
        self.assertFalse(output.exists())
        self.dll.write_bytes(b'synthetic registered DLL, never executable')
        second = self.dll.with_name('second.dll')
        second.write_bytes(self.dll.read_bytes())
        with self.assertRaisesRegex(ValueError, 'exactly one fixed'):
            stage()
        second.unlink()
        (self.notices / 'LICENSE.txt').unlink()
        with self.assertRaisesRegex(ValueError, 'licenses/LICENSE.txt'):
            stage()
        (self.notices / 'LICENSE.txt').write_bytes(b'fixture')
        second.symlink_to(self.dll)
        with self.assertRaisesRegex(ValueError, 'symlink'):
            stage()
        self.assertFalse(output.exists())

    def test_copy_race_fails_without_publishing_manifest(self):
        runtime = self.make_runtime()
        output = self.root / 'race'
        original_copy = pack.copy
        def corrupt(source, destination):
            original_copy(source, destination)
            if destination.as_posix().endswith('/resources/openblas/openblas.dll'):
                destination.write_bytes(b'changed during copy')
        with patch.object(pack.sys, 'platform', 'win32'), patch.object(pack, 'copy', side_effect=corrupt):
            with self.assertRaisesRegex(ValueError, 'changed while staging'):
                pack.stage(output, self.binaries, research=True, iqdb=self.iqdb, openblas_site=runtime)
        self.assertFalse((output / 'manifest.json').exists())
        portable = self.root / 'good portable'
        desktop = self.root / 'race desktop'
        with patch.object(pack.sys, 'platform', 'win32'):
            pack.stage(portable, self.binaries, research=True, iqdb=self.iqdb, models=self.models,
                       openblas_site=runtime, ort_dylib=self.ort, ort_licenses=self.ort_licenses,
                       directml=self.directml)
            with patch.object(pack, 'copy', side_effect=corrupt), self.assertRaisesRegex(ValueError, 'changed while staging desktop'):
                pack.desktop_compute(portable, desktop)
        self.assertFalse((desktop / 'manifest.json').exists())
