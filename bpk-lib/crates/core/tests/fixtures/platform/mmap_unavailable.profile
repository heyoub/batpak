{
  "schema_version": 1,
  "host": {
    "monotonic_clock": "ProcessLocalInstantAnchor"
  },
  "store_path": {
    "path_status": "ObservedDirectory",
    "parent_dir_sync": "UnixFsync",
    "lock_leaf_symlink_protection": "AtomicNoFollow",
    "mmap_index": "ObservedUnsupported",
    "sealed_segment_mmap": "ObservedUnsupported",
    "active_segment_read": "UnixReadAt"
  },
  "admission": {
    "store_lock": "AtomicNoFollow",
    "parent_dir_sync": "UnixFsync",
    "mmap_index": "Rejected",
    "sealed_segment_mmap": "Rejected"
  },
  "fingerprint_crc32": 1810418857
}
