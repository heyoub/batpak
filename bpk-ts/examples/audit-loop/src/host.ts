import { createConnection, type Socket } from "node:net";

import {
  BankCommitAck,
  BankCommitRequest,
  BANK_COMMIT,
  call,
  decodeBytes,
  encodeBytes,
  encodeHex,
  EventGetAck,
  EventGetRequest,
  EVENT_GET,
  EventQueryAck,
  EventQueryRequest,
  EVENT_QUERY,
  type EventSummary,
} from "@batpak/sdk";

import { DEMO_ENTITY, DEMO_KIND_CATEGORY, DEMO_SCOPE } from "./constants.js";

async function openSocket(host: string, port: number): Promise<Socket> {
  return new Promise((resolveSocket, reject) => {
    const socket = createConnection({ host, port }, () => resolveSocket(socket));
    socket.once("error", reject);
  });
}

async function withSocket<T>(
  host: string,
  port: number,
  run: (socket: Socket) => Promise<T>,
): Promise<T> {
  const socket = await openSocket(host, port);
  try {
    return await run(socket);
  } finally {
    socket.end();
  }
}

export async function commitAppEvent(
  host: string,
  port: number,
  kindTypeId: number,
  schema: Parameters<typeof encodeBytes>[0],
  value: Parameters<typeof encodeBytes>[1],
): Promise<typeof BankCommitAck.Type> {
  const payload = encodeBytes(schema, value);
  const request: typeof BankCommitRequest.Type = {
    entity: DEMO_ENTITY,
    scope: DEMO_SCOPE,
    kind_category: DEMO_KIND_CATEGORY,
    kind_type_id: kindTypeId,
    payload_hex: encodeHex(payload) as typeof BankCommitRequest.Type.payload_hex,
    // Optional durable-idempotency operation key (manifest v2, additive); unset here.
    idempotency_key_hex: null,
  };

  return withSocket(host, port, async (socket) => {
    const response = await call(socket, BANK_COMMIT.name, encodeBytes(BankCommitRequest, request));
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `bank.commit: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    return decodeBytes(BankCommitAck, response.output);
  });
}

export async function queryAuditSummariesByGlobalSequence(
  host: string,
  port: number,
): Promise<readonly EventSummary[]> {
  let after_global_sequence: (typeof EventQueryRequest.Type)["after_global_sequence"] = null;
  const entries: EventSummary[] = [];

  for (;;) {
    const request: typeof EventQueryRequest.Type = {
      entity: DEMO_ENTITY,
      scope: DEMO_SCOPE,
      kind_category: null,
      kind_type_id: null,
      after_global_sequence,
      limit: 64,
    };

    const page = await withSocket(host, port, async (socket) => {
      const response = await call(
        socket,
        EVENT_QUERY.name,
        encodeBytes(EventQueryRequest, request),
      );
      if (response.kind !== "netbat-ok") {
        throw new Error(
          `event.query: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
        );
      }
      return decodeBytes(EventQueryAck, response.output);
    });

    entries.push(...page.entries);
    if (!page.truncated) {
      return entries;
    }
    if (page.next_after_global_sequence === null) {
      throw new Error("event.query: truncated page did not provide next_after_global_sequence");
    }
    after_global_sequence = page.next_after_global_sequence;
  }
}

export async function getEvent(
  host: string,
  port: number,
  eventIdHex: string,
): Promise<typeof EventGetAck.Type> {
  const request: typeof EventGetRequest.Type = {
    event_id_hex: eventIdHex as typeof EventGetRequest.Type.event_id_hex,
  };

  return withSocket(host, port, async (socket) => {
    const response = await call(socket, EVENT_GET.name, encodeBytes(EventGetRequest, request));
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `event.get: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    return decodeBytes(EventGetAck, response.output);
  });
}
