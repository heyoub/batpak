{
  "schema_version": 1,
  "host": {
    "monotonic_clock": "ProcessLocalInstantAnchor"
  },
  "store_path": {
    "path_status": "ObservedDirectory",
    "parent_dir_sync": "UnixFsync",
    "lock_leaf_symlink_protection": "AtomicNoFollow",
    "mmap_index": "FileBacked",
    "sealed_segment_mmap": "FileBacked",
    "active_segment_read": "UnixReadAt"
  },
  "admission": {
    "store_lock": "AtomicNoFollow",
    "parent_dir_sync": "UnixFsync",
    "mmap_index": "FileBacked",
    "sealed_segment_mmap": "FileBacked"
  },
  "fingerprint_crc32": 198181094
}