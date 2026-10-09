"""Explicit one-slot consumer contract; neither a probe updater nor a private inventory reader."""
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import importlib.metadata
import json
import os
from pathlib import Path
import re
import subprocess
import time
import tomllib
import uuid


def load(path):
    path = Path(path).resolve()
    raw = path.read_bytes()
    data = tomllib.loads(raw.decode())
    if set(data) != {'schema_version', 'example', 'probe', 'target', 'flash'}:
        raise ValueError('unknown or missing equipment sections')
    if type(data['schema_version']) is not int or data['schema_version'] != 1 or type(data['example']) is not bool:
        raise ValueError('unsupported schema or invalid example flag')
    for section, required, optional in [('probe', {'port', 'unit_id', 'selector'}, set()),
            ('target', {'chip', 'chip_id'}, {'uid'}), ('flash', {'image', 'address'}, set())]:
        row = data[section]
        if not isinstance(row, dict) or not required <= row.keys() or row.keys() - required - optional:
            raise ValueError('invalid ' + section + ' fields')
    probe = data['probe']
    if not isinstance(probe['port'], str) or not probe['port'].startswith(('/dev/serial/by-id/', '/run/board-identify/by-id/')):
        raise ValueError('an explicit stable serial path is required')
    if not re.fullmatch(r'[0-9a-fA-F]{12,32}', probe['unit_id']):
        raise ValueError('complete OEP unit ID required')
    prefix = 'port:oep://' + probe['unit_id'] + '/'
    if not isinstance(probe['selector'], str) or not probe['selector'].startswith(prefix) or not re.fullmatch(r'[A-Za-z0-9_-]+', probe['selector'][len(prefix):]):
        raise ValueError('selector must name this OEP unit and an explicit installed slot')
    if not isinstance(data['target']['chip'], str) or not re.fullmatch(r'0x[0-9a-fA-F]{8}', data['target']['chip_id']):
        raise ValueError('expected target chip/SKU required')
    if 'uid' in data['target'] and not re.fullmatch(r'[0-9a-fA-F]+', data['target']['uid']):
        raise ValueError('invalid expected target UID')
    flash = data['flash']
    if type(flash['address']) is not int or flash['address'] != 0x08000000:
        raise ValueError('this code Flash contract requires address 0x08000000')
    if not isinstance(flash['image'], str) or not flash['image']:
        raise ValueError('explicit binary test image required')
    image = Path(flash['image'])
    flash['image'] = str((path.parent / image).resolve() if not image.is_absolute() else image)
    data['configuration_path'] = str(path)
    data['configuration_sha256'] = hashlib.sha256(raw).hexdigest()
    return data


@contextmanager
def shared_lock(path):
    import fcntl
    with Path(path).open('r+') as handle:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        try:
            yield
        finally:
            fcntl.flock(handle, fcntl.LOCK_UN)


def check_target(expected, info):
    target = info.get('target') or info.get('result', {}).get('target', {})
    if not info.get('ok') or target.get('sku') != expected['chip'] or target.get('chip_id', '').lower() != expected['chip_id'].lower():
        raise ValueError('DUT SKU/chip ID differs from explicit configuration')
    if 'uid' in expected and (target.get('uid') or '').lower() != expected['uid'].lower():
        raise ValueError('DUT UID differs from explicit configuration')
    return target


def probe_snapshot(probe):
    from oep_client import core, dump, link, registry
    host = link.open_host(probe['port'], keep_session=False)
    try:
        tags = registry.CORE.tlv['describe']
        values = dict(core.describe(host))
        result = {key: values.get(tags[key], b'').decode('utf-8') or None for key in ('unit_id', 'firmware', 'model', 'chip')}
        if not result['unit_id'] or result['unit_id'].lower() != probe['unit_id'].lower():
            raise ValueError('OEP unit ID mismatch; target operations refused')
        declarations = dump.collect(lambda fn, op, payload: host.call(fn, op, payload, locked=False).payload,
                                    confirm=host.confirm_range())
        result['declarations'] = json.loads(dump.to_json(declarations))
        if declarations.missing:
            raise ValueError('missing OEP declarations: ' + ', '.join(declarations.missing))
        return result
    finally:
        host.link.close()


def run(config, tool, lock, results):
    if config['example']:
        raise ValueError('example configuration cannot operate physical hardware')
    tool = Path(tool).resolve(strict=True)
    image = Path(config['flash']['image'])
    payload = image.read_bytes()
    if image.suffix != '.bin' or not payload:
        raise ValueError('this contract requires a nonempty raw binary test image')
    output = Path(results) / ('ch32rv-' + datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S%fZ') + '-' + uuid.uuid4().hex[:8])
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for name in ('CH32RV_PROBE', 'CH32RV_CHIP', 'CH32RV_DB', 'CH32RV_REPLAY', 'CH32RV_CAPTURE'):
        env.pop(name, None)
    runtime = output / 'runtime'
    runtime.mkdir(mode=0o700)
    env['XDG_RUNTIME_DIR'] = str(runtime.resolve())
    report = {'status': 'failed', 'contract': 'TOOL-FLASH', 'configuration': config,
              'started_at': datetime.now(timezone.utc).isoformat(), 'commands': [],
              'image_sha256': hashlib.sha256(payload).hexdigest(), 'image_bytes': len(payload),
              'client_version': importlib.metadata.version('oep-client-python'),
              'tool_path': str(tool), 'tool_sha256': hashlib.sha256(tool.read_bytes()).hexdigest(),
              'probe_firmware_update': False, 'flash_backup': False, 'firmware_restore': False,
              'target_individual_identity': 'uid' if 'uid' in config['target'] else 'chip identity only; fixture assignment operator-confirmed'}
    prefix = [str(tool), '--probe', config['probe']['selector'], '--chip', config['target']['chip'], '--json', '--non-interactive']

    def command(name, args):
        argv = prefix + args
        report['commands'].append({'name': name, 'argv': argv})
        proc = subprocess.run(argv, env=env, capture_output=True, text=True, timeout=120)
        (output / (name + '.stdout')).write_text(proc.stdout)
        (output / (name + '.stderr')).write_text(proc.stderr)
        if proc.returncode:
            raise RuntimeError(f'{name} exit {proc.returncode}: {proc.stdout[-1500:]} {proc.stderr[-500:]}')
        value = json.loads(proc.stdout)
        if not value.get('ok'):
            raise RuntimeError(f'{name} did not report success: {value}')
        return value

    try:
        with shared_lock(lock):
            report['probe'] = probe_snapshot(config['probe'])
            report['tool_version'] = command('version', ['version'])
            report['target_before'] = check_target(config['target'], command('target-info', ['target', 'info']))
            target_checked = True
            try:
                report['flash'] = command('flash', ['flash', str(image), '--at', hex(config['flash']['address']), '--verify', 'readback', '--confirm-run', 'status'])
                readback = output / 'readback.bin'
                command('readback', ['read', '--range', f"{hex(config['flash']['address'])}+{len(payload)}", '-o', str(readback.resolve())])
                contents = readback.read_bytes()
                report['readback_sha256'] = hashlib.sha256(contents).hexdigest()
                if contents != payload:
                    raise RuntimeError('readback differs from the exact test image')
                report['status'] = 'passed'
            finally:
                if target_checked:
                    try:
                        report['cleanup'] = command('reset-run', ['reset', '--confirm-run', 'status'])
                    except Exception as error:
                        report['cleanup_error'] = str(error)
                        report['status'] = 'failed'
    except Exception as error:
        report['error'] = str(error)
    finally:
        report['finished_at'] = datetime.now(timezone.utc).isoformat()
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
    return report, output / 'result.json'
