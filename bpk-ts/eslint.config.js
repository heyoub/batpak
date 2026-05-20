// ESLint flat config (ESLint 9+) for the BatPAK TypeScript SDK
// workspace.
//
// Strategy:
//   - Production src/** in every package gets type-aware linting
//     (recommendedTypeChecked) plus the strict no-explicit-any /
//     no-floating-promises / no-misused-promises bundle.
//   - Tests get the standard recommended preset (no type-aware rules)
//     so they don't require the per-package tsconfig to include
//     `test/**` — keeps `composite: true` builds simple.

import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "**/dist/**",
      "**/node_modules/**",
      "**/coverage/**",
      "packages/generated/src/**", // auto-generated, formatted by the codegen
    ],
  },
  js.configs.recommended,

  // Production src/** lanes — type-aware.
  {
    files: ["packages/*/src/**/*.ts", "examples/*/src/**/*.ts"],
    extends: [...tseslint.configs.recommendedTypeChecked],
    languageOptions: {
      parserOptions: {
        projectService: true,
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/no-misused-promises": "error",
      "@typescript-eslint/no-unsafe-argument": "warn",
      "@typescript-eslint/no-unsafe-assignment": "warn",
      "@typescript-eslint/no-unsafe-call": "warn",
      "@typescript-eslint/no-unsafe-member-access": "warn",
      "@typescript-eslint/no-unsafe-return": "warn",
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
      "@typescript-eslint/consistent-type-imports": "error",
    },
  },

  // Test lane — non-type-aware. Tests construct values from `unknown`
  // JSON and the unsafe-* rules add friction without value.
  {
    files: ["packages/*/test/**/*.ts", "**/*.test.ts"],
    extends: [...tseslint.configs.recommended],
    rules: {
      "@typescript-eslint/no-unused-vars": [
        "error",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
        },
      ],
    },
  },
);
