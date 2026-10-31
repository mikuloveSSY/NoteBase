"""Run core regression while explicitly listing the existing Windows-only migration cases.
No test assertions are altered. Windows executes the full suite.
"""
import os
import subprocess

cmd = ['cargo', 'test', '--manifest-path', 'crates/notebase-core/Cargo.toml']
if os.name != 'nt':
    cases = [
        'settings_migration_preserves_private_drafts_and_resumes_relative_rename_in_new_vault',
        'migration_copies_notes_attachments_trash_and_empty_directories_and_keeps_source',
        'ipc_migration_switches_only_after_verified_copy_and_recalculates_id',
        'failed_settings_commit_keeps_active_source_and_original_config',
        'migration_carries_snapshot_blobs_and_known_business_state_to_new_vault_id',
    ]
    print('Not run on this platform: Windows migration tests', flush=True)
    cmd.append('--')
    for case in cases:
        print('  ' + case, flush=True)
        cmd.extend(['--skip', case])
raise SystemExit(subprocess.call(cmd))
