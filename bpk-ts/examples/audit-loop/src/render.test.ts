import { describe, expect, it } from "vitest";

import { formatStream, sortRowsByGlobalSequence, type StreamRow } from "./render.js";

describe("render stream ordering", () => {
  it("orders rows by global_sequence regardless of input order", () => {
    const rows: StreamRow[] = [
      { globalSequence: 3, kindLabel: "chat", body: "third" },
      { globalSequence: 1, kindLabel: "note", body: "first" },
      { globalSequence: 2, kindLabel: "task", body: "second" },
    ];

    expect(sortRowsByGlobalSequence(rows).map((row) => row.globalSequence)).toEqual([1, 2, 3]);
    expect(formatStream(rows)).toEqual([
      "1. seq=1 note: first",
      "2. seq=2 task: second",
      "3. seq=3 chat: third",
    ]);
  });

  it("keeps stable ordering when global_sequence is already sorted", () => {
    const rows: StreamRow[] = [
      { globalSequence: 1, kindLabel: "note", body: "alpha" },
      { globalSequence: 2, kindLabel: "task", body: "beta" },
    ];

    expect(formatStream(rows)).toEqual([
      "1. seq=1 note: alpha",
      "2. seq=2 task: beta",
    ]);
  });
});
