import type { EventGetAck, EventSummary } from "@batpak/generated";

import { decodePayload, kindLabel } from "./events.js";

export interface StreamRow {
  globalSequence: number;
  kindLabel: string;
  body: string;
}

export function rowFromSubstrate(summary: EventSummary, event: EventGetAck): StreamRow {
  return {
    globalSequence: summary.global_sequence,
    kindLabel: kindLabel(summary.kind_category, summary.kind_type_id),
    body: decodePayload(event.kind_category, event.kind_type_id, event.payload_hex),
  };
}

/** Stable ordering by substrate global_sequence (commit order, not wall clock or hash-chain order). */
export function sortRowsByGlobalSequence(rows: readonly StreamRow[]): StreamRow[] {
  return [...rows].sort((left, right) => left.globalSequence - right.globalSequence);
}

export function formatStream(rows: readonly StreamRow[]): string[] {
  return sortRowsByGlobalSequence(rows).map(
    (row, index) => `${index + 1}. seq=${row.globalSequence} ${row.kindLabel}: ${row.body}`,
  );
}

export function printStream(lines: readonly string[]): void {
  console.log("audit-loop: stream begin");
  for (const line of lines) {
    console.log(line);
  }
  console.log("audit-loop: stream end");
}
