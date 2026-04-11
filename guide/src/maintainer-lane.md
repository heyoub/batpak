# Maintainer lane

The canonical repo workflow is:

```bash
cargo xtask doctor
cargo xtask ci
cargo xtask docs
```

For performance work:

```bash
cargo xtask bench --surface neutral
cargo xtask bench --surface native --save
cargo xtask bench --surface neutral --compare
```
