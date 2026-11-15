"""以原生 CLI 准备合成认领夹具；算法期望由既有固定契约单独验收。"""
import json
from pathlib import Path
import subprocess
from tools.wait_limit import WAIT


def claims(binary, root, shared, source, identity, assets, features, env):
    # 该路径只准备端到端发布输入；不产生或刷新提交的算法期望。
    request = dict(mode='claims', shared=str(shared), source=str(source),
                   identity=identity, assets=assets, features=features.tolist(),
                   tamper_asset=assets[0], tamper_character='not-an-index-character')
    input_path = Path(root) / 'fixture-claims.json'
    output_path = Path(root) / 'fixture-claims-output.json'
    input_path.write_text(json.dumps(request), encoding='utf-8')
    output_path.unlink(missing_ok=True)
    result = subprocess.run([str(binary), str(input_path), str(output_path)],
                            env=env, capture_output=True, timeout=WAIT * 2)
    if result.returncode:
        raise AssertionError(result.stderr.decode(errors='replace'))
    value = json.loads(output_path.read_text(encoding='utf-8'))
    if not value['claims'] or not value['tampered_rejected']:
        raise AssertionError('合成夹具必须含可认领结果并拒绝篡改')
    return value
