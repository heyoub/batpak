{
  "schema_version": 1,
  "host": {
    "monotonic_clock": "ProcessLocalInstantAnchor"
  },
  "store_path": {
    "path_status": "ObservedDirectory",
    "parent_dir_sync": "RenameOnly",
    "lock_leaf_symlink_protection": "BestEffortCheckThenOpen",
    "mmap_index": "FileBacked",
    "sealed_segment_mmap": "FileBacked",
    "active_segment_read": "LockedSeekRead"
  },
  "admission": {
    "store_lock": "BestEffortCheckThenOpen",
    "parent_dir_sync": "RenameOnly",
    "mmap_index": "FileBacked",
    "sealed_segment_mmap": "FileBacked"
  },
  "fingerprint_crc32": 352735033
}
