/**
 * `@batpak/sdk` — one-install entry for NETBAT/1 TypeScript consumers.
 *
 * Re-exports the four publishable packages so apps can depend on a single
 * dependency instead of wiring `@batpak/client`, `@batpak/schema`,
 * `@batpak/generated`, and `@batpak/canonical` separately.
 */

export * from "@batpak/canonical";
export * from "@batpak/client";
export * from "@batpak/schema";
export * from "@batpak/generated";
