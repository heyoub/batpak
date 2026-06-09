import { Schema, bank, decodeBytes } from "@batpak/sdk";

import {
  DEMO_KIND_CATEGORY,
  KIND_TYPE_CHAT,
  KIND_TYPE_NOTE,
  KIND_TYPE_TASK,
} from "./constants.js";

// App-owned demo payloads only. `task`, `assignee`, and `chat` are example
// fields above the substrate, not BatPAK workflow semantics or NETBAT/1 ops.
export const NotePosted = bank.event({
  title: Schema.String,
  body: Schema.String,
});

export const TaskOpened = bank.event({
  title: Schema.String,
  assignee: Schema.String,
});

export const ChatLine = bank.event({
  speaker: Schema.String,
  text: Schema.String,
});

export type NotePosted = typeof NotePosted.Type;
export type TaskOpened = typeof TaskOpened.Type;
export type ChatLine = typeof ChatLine.Type;

export function kindLabel(kindCategory: number, kindTypeId: number): string {
  if (kindCategory !== DEMO_KIND_CATEGORY) {
    return `kind:${kindCategory}/${kindTypeId}`;
  }
  switch (kindTypeId) {
    case KIND_TYPE_NOTE:
      return "note";
    case KIND_TYPE_TASK:
      return "task";
    case KIND_TYPE_CHAT:
      return "chat";
    default:
      return `app:${kindTypeId}`;
  }
}

export function decodePayload(
  kindCategory: number,
  kindTypeId: number,
  payloadHex: string,
): string {
  const bytes = hexToBytes(payloadHex);
  if (kindCategory !== DEMO_KIND_CATEGORY) {
    return `<opaque ${bytes.length} bytes>`;
  }
  switch (kindTypeId) {
    case KIND_TYPE_NOTE: {
      const note = decodeBytes(NotePosted, bytes);
      return `${note.title} — ${note.body}`;
    }
    case KIND_TYPE_TASK: {
      const task = decodeBytes(TaskOpened, bytes);
      return `${task.title} (@${task.assignee})`;
    }
    case KIND_TYPE_CHAT: {
      const chat = decodeBytes(ChatLine, bytes);
      return `${chat.speaker}: ${chat.text}`;
    }
    default:
      return `<unknown app kind ${kindTypeId}>`;
  }
}

function hexToBytes(hex: string): Uint8Array {
  if (hex.length === 0) {
    return new Uint8Array();
  }
  const pairs = hex.match(/.{1,2}/gu);
  if (!pairs) {
    throw new Error(`invalid payload hex ${JSON.stringify(hex)}`);
  }
  return new Uint8Array(pairs.map((pair) => parseInt(pair, 16)));
}
