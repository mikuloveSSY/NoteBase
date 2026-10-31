"""Verify a NoteBase v1 ZIP without the app. Does not extract or modify files.
Usage: python scripts/verify_backup.py /path/to/backup.zip
"""
import hashlib
import json
import sys
import zipfile
from pathlib import PurePosixPath


def verify(path):
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        if len(names) != len(set(names)):
            raise ValueError('duplicate ZIP entry')
        manifest = json.loads(archive.read('manifest.json'))
        if manifest['formatVersion'] != 1:
            raise ValueError('unsupported format')
        items = sorted(manifest['files'], key=lambda item: item['path'])
        if len({item['path'] for item in items}) != len(items):
            raise ValueError('duplicate manifest path')
        combined = hashlib.sha256()
        expected = {'manifest.json'}
        for item in items:
            relative = item['path']
            path = PurePosixPath(relative)
            if path.is_absolute() or '..' in path.parts or '\\' in relative:
                raise ValueError('unsafe path')
            name = relative if relative.startswith('attachments/') else 'notes/' + relative
            expected.add(name)
            content = hashlib.sha256()
            with_path = hashlib.sha256(relative.encode() + b'\0')
            size = 0
            with archive.open(name) as stream:
                while chunk := stream.read(65536):
                    size += len(chunk)
                    content.update(chunk)
                    with_path.update(chunk)
            if size != item['size'] or content.hexdigest() != item['sha256']:
                raise ValueError(f'file checksum mismatch: {relative}')
            combined.update(with_path.hexdigest().encode())
        if set(names) != expected or combined.hexdigest() != manifest['checksum']:
            raise ValueError('package checksum or file list mismatch')
        print('Verified', len(items), 'files', manifest['checksum'])


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('usage: python verify_backup.py BACKUP.zip')
    verify(sys.argv[1])
