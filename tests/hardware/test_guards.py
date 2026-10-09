import fcntl
from pathlib import Path

import pytest

from harness import check_target, load, run, shared_lock


def test_example_cannot_open_hardware(tmp_path):
    config = load(Path(__file__).with_name('equipment.example.toml'))
    with pytest.raises(ValueError, match='example'):
        run(config, '/missing-tool', '/missing-lock', tmp_path / 'results')
    assert not (tmp_path / 'results').exists()


def test_selector_cannot_choose_another_probe(tmp_path):
    source = Path(__file__).with_name('equipment.example.toml').read_text()
    path = tmp_path / 'equipment.toml'
    path.write_text(source.replace('oep://000000000000/', 'oep://111111111111/'))
    with pytest.raises(ValueError, match='selector'):
        load(path)


def test_target_identity_mismatch_is_rejected():
    with pytest.raises(ValueError, match='SKU/chip'):
        check_target({'chip': 'CH32L103C8T6', 'chip_id': '0x10310710'},
                     {'ok': True, 'target': {'sku': 'CH32V003F4U6', 'chip_id': '0x00310510'}})


def test_lock_is_shared_and_never_created(tmp_path):
    path = tmp_path / 'equipment.lock'
    with pytest.raises(FileNotFoundError):
        with shared_lock(path):
            pass
    path.touch()
    inode = path.stat().st_ino
    with path.open('r+') as owner:
        fcntl.flock(owner, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with pytest.raises(BlockingIOError):
            with shared_lock(path):
                pass
    with shared_lock(path):
        assert path.stat().st_ino == inode


@pytest.mark.parametrize('failure,expected_commands', [
    ('identity', ['version', 'target', 'info']),
    ('flash', ['version', 'target', 'info', 'flash', 'reset']),
])
def test_failure_records_evidence_and_only_resets_identified_target(tmp_path, monkeypatch, failure, expected_commands):
    import json
    import subprocess
    import harness

    config = load(Path(__file__).with_name('equipment.example.toml'))
    config['example'] = False
    image = tmp_path / 'candidate.bin'
    image.write_bytes(b'candidate')
    config['flash']['image'] = str(image)
    tool = tmp_path / 'tool'
    tool.touch()
    lock = tmp_path / 'lock'
    lock.touch()
    monkeypatch.setattr(harness, 'probe_snapshot', lambda _: {'unit_id': '000000000000'})
    operations = []

    def fake_command(argv, **kwargs):
        args = argv[7:]
        operations.extend(args[:2] if args[0] == 'target' else args[:1])
        value = {'ok': True}
        code = 0
        if args[0] == 'target':
            value['target'] = {'sku': 'other' if failure == 'identity' else 'CH32L103C8T6', 'chip_id': '0x10310710'}
        if args[0] == 'flash':
            code = 1
            value = {'ok': False, 'error': 'injected flash failure'}
        return subprocess.CompletedProcess(argv, code, json.dumps(value), '')

    monkeypatch.setattr(harness.subprocess, 'run', fake_command)
    report, artifact = run(config, tool, lock, tmp_path / 'results')
    assert report['status'] == 'failed'
    assert operations == expected_commands
    assert json.loads(artifact.read_text())['error']
    assert ('cleanup' in report) == (failure == 'flash')
