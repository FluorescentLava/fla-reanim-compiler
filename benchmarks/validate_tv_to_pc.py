"""Independent comparison of native TV->PC against the preserved legacy tool."""
import argparse
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
from pathlib import Path
import platform
import struct
import subprocess
import tempfile
import zlib


COOKIE, SCHEMA = 0xDEADFED4, 0xB393B4C0
BASELINE_GIT_SHA256 = '465f1af93464b32970375250198bd6a8956ab145b68cf7066dbc444bde4f4bde'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def unpack(data):
    assert len(data) >= 8 and struct.unpack_from('<I', data)[0] == COOKIE
    declared = struct.unpack_from('<I', data, 4)[0]
    decoder = zlib.decompressobj()
    body = decoder.decompress(data[8:])
    assert decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail
    assert len(body) == declared
    return body


def execute(cli, source, output):
    command = [str(cli), '--tv-to-pc', '-o', str(output), str(source)]
    run = subprocess.run(command, capture_output=True, timeout=120, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
    return command, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--rust', type=Path, required=True)
    parser.add_argument('--baseline', type=Path, required=True)
    parser.add_argument('--source-dir', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    for field in ['rust', 'baseline', 'source_dir']:
        setattr(args, field, getattr(args, field).resolve(strict=True))
    args.output = args.output.resolve()
    project = Path(__file__).resolve().parents[1]
    (project / '.qa').mkdir(exist_ok=True)
    normalized_baseline_sha = hashlib.sha256(args.baseline.read_bytes().replace(b'\r\n', b'\n')).hexdigest()
    if normalized_baseline_sha != BASELINE_GIT_SHA256:
        raise ValueError('Baseline does not match the frozen legacy Git blob after LF normalization')
    work = Path(tempfile.mkdtemp(prefix='tv-to-pc-validation-', dir=project / '.qa'))
    spec = importlib.util.spec_from_file_location('preserved_legacy_tv_to_pc', args.baseline)
    legacy = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(legacy)
    result = {'schema_version': 1, 'created_utc': datetime.now(timezone.utc).isoformat(),
              'completed': False, 'purpose': 'TV->PC binary correctness; not a performance benchmark',
              'platform': platform.platform(), 'python': platform.python_version(),
              'binary': {'path': str(args.rust), 'sha256': sha(args.rust), 'bytes': args.rust.stat().st_size},
              'baseline': {'path': str(args.baseline), 'sha256': sha(args.baseline),
                           'git_blob_sha256': BASELINE_GIT_SHA256,
                           'lf_normalized_sha256': normalized_baseline_sha,
                           'git_revision': '939af14be31aeca93fb842addbbbd0a262692ae2',
                           'git_resource': 'tools/tv2pc_reanim.py'},
              'validator_sha256': sha(Path(__file__)), 'tv_module_sha256': sha(project / 'src/tv.rs'),
              'tv_tests_sha256': sha(project / 'tests/tv.rs'), 'source_dir': str(args.source_dir),
              'work_dir': str(work), 'policies': {
                  'comparison': 'exact complete decompressed PC body bytes; compressed zlib representation need not match',
                  'wrapper': 'cookie, declared size, completed zlib stream and full consumption checked independently',
                  'schema': 'PC 0xB393B4C0, RDef 12, track/frame statistics match the legacy decoder',
                  'strings': 'legacy latin-1 roundtrip preserves original bytes, including non-UTF-8; native synthetic tests cover high bytes and NUL',
                  'fields': 'eight original f32 words and MISSING preserved; only iPath/i2/i2Path and serialized pointers removed',
                  'game_loader': 'not run by this comparison; game resource registration is a separate concern'},
              'samples': [], 'negative_controls': []}
    files = sorted(args.source_dir.glob('*.reanim.compiled'))
    assert files
    for index, source in enumerate(files):
        old_output, native_output = work / f'old-{index}.compiled', work / f'native-{index}.compiled'
        tracks, fps = legacy.tv_decode(source)
        legacy.pc_encode(tracks, fps, old_output)
        command, run = execute(args.rust, source, native_output)
        sample = {'input': str(source), 'input_sha256': sha(source), 'input_bytes': source.stat().st_size,
                  'command': command, 'exit_code': run.returncode, 'tracks': len(tracks), 'fps': fps,
                  'frames': max((track['count'] for track in tracks), default=0),
                  'frame_records': sum(track['count'] for track in tracks)}
        if run.returncode:
            sample.update({'passed': False, 'error': run.stderr.decode('utf-8', 'replace')})
        else:
            old, native = unpack(old_output.read_bytes()), unpack(native_output.read_bytes())
            schema, _, count, fps_bits, _, marker = struct.unpack_from('<6I', native)
            cli_report = json.loads(run.stdout.decode('utf-8').splitlines()[-1])
            sample.update({'output_sha256': sha(native_output), 'legacy_output_sha256': sha(old_output),
                           'output_body_sha256': hashlib.sha256(native).hexdigest(),
                           'legacy_body_sha256': hashlib.sha256(old).hexdigest(), 'body_bytes': len(native),
                           'body_bytes_equal': native == old, 'schema': f'0x{schema:08X}',
                           'rdef_marker': marker, 'cli_report': cli_report,
                           'stats_match': count == len(tracks) and fps_bits == struct.unpack('<I', struct.pack('<f', fps))[0]
                             and cli_report['tracks'] == len(tracks) and cli_report['frames'] == sample['frames']
                             and struct.pack('<f', cli_report['fps']) == struct.pack('<f', fps)})
            sample['passed'] = native == old and schema == SCHEMA and marker == 12 and sample['stats_match']
        result['samples'].append(sample)
        if not sample['passed']:
            print('FAIL', source.name, sample.get('error', 'body/stats differ'), flush=True)
        if (index + 1) % 25 == 0 or index + 1 == len(files):
            print(f'Checked {index+1}/{len(files)}', flush=True)
    first_tv = files[0].read_bytes()
    raw = unpack(first_tv)
    raw_source = work / 'raw-tv.reanim.compiled'
    raw_source.write_bytes(raw)
    raw_output = work / 'raw-pc.reanim.compiled'
    _, run = execute(args.rust, raw_source, raw_output)
    result['raw_tv_control'] = {'exit_code': run.returncode,
         'body_matches_wrapped': run.returncode == 0 and unpack(raw_output.read_bytes()) == unpack((work / 'native-0.compiled').read_bytes())}
    size_wrong = bytearray(first_tv)
    struct.pack_into('<I', size_wrong, 4, len(raw) + 1)
    controls = {'already_pc': (work / 'native-0.compiled').read_bytes(), 'wrong_declared_size': size_wrong,
                'truncated_zlib': first_tv[:-1], 'wrapped_trailing': first_tv + b'\0', 'raw_trailing': raw + b'\0'}
    for name, data in controls.items():
        source, output = work / (name + '.compiled'), work / (name + '-output.compiled')
        source.write_bytes(data)
        _, run = execute(args.rust, source, output)
        result['negative_controls'].append({'name': name, 'exit_code': run.returncode,
            'output_exists': output.exists(), 'error': run.stderr.decode('utf-8', 'replace'),
            'passed': run.returncode != 0 and not output.exists()})
    result['summary'] = {'input_files': len(files), 'passed_files': sum(sample['passed'] for sample in result['samples']),
        'body_bytes_equal_files': sum(sample.get('body_bytes_equal', False) for sample in result['samples']),
        'pc_schema_correct_files': sum(sample.get('schema') == '0xB393B4C0' for sample in result['samples']),
        'stats_equal_files': sum(sample.get('stats_match', False) for sample in result['samples']),
        'tracks': sum(sample['tracks'] for sample in result['samples']),
        'frame_records': sum(sample['frame_records'] for sample in result['samples']),
        'negative_controls_passed': sum(control['passed'] for control in result['negative_controls'])}
    result['binary_unchanged'] = sha(args.rust) == result['binary']['sha256']
    result['baseline_unchanged'] = sha(args.baseline) == result['baseline']['sha256']
    result['completed'] = True
    result['passed'] = (all(sample['passed'] for sample in result['samples'])
        and result['raw_tv_control']['body_matches_wrapped'] and all(control['passed'] for control in result['negative_controls'])
        and result['binary_unchanged'] and result['baseline_unchanged'])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2), encoding='utf-8')
    print(json.dumps(result['summary'], indent=2), flush=True)
    raise SystemExit(0 if result['passed'] else 1)


if __name__ == '__main__':
    main()
