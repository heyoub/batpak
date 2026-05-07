# batpak-macros-support

Shared support crate for batpak's procedural macros.

This crate is published so `batpak` and `batpak-macros` can resolve from crates.io. It is not intended as a direct public API surface; use the `batpak` crate instead.

Keep this crate's version matched exactly to the root `batpak` crate version; mixing versions can produce confusing derive and trait-resolution errors.

Repository: <https://github.com/TheFreeBatteryFactory/batpak>
