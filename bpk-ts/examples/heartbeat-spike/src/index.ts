/**
 * 0.8.2 live integration spike.
 *
 * Boots against a running `hbat` on `--port N` and exercises the live
 * calibration path: heartbeat + commit/query/get + typed ERR. The full
 * ten-operation NETBAT/1 host profile also includes `receipt.verify`,
 * `event.walk`, and the four `evidence.*` ops, which are covered by
 * manifest/parity and hbat tests.
 *
 *   1. `system.heartbeat`  — proves the wire is open.
 *   2. `bank.commit`        — appends a typed event, returns AppendReceipt.
 *   3. `event.query`        — pages metadata by coordinate and global sequence.
 *   4. `event.get`          — reads the event back by event_id; the
 *                              payload bytes round-trip back into the
 *                              original Rust-typed struct via Effect 4.
 *   5. Error path           — unknown_operation returns typed NetbatError.
 *
 * Boot `hbat` separately:
 *
 *   cargo run -p hbat -- serve --store $(mktemp -d) --tcp 127.0.0.1:0 --print-port
 *
 * Parse the `HBAT_READY {"port": N, ...}` line on stdout, then invoke:
 *
 *   pnpm --filter @batpak/example-heartbeat-spike start -- --port N
 */

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
  SystemHeartbeatAck,
  SystemHeartbeatRequest,
  SYSTEM_HEARTBEAT,
} from "@batpak/sdk";

interface CliArgs {
  port: number;
  host: string;
}

function parseArgs(argv: readonly string[]): CliArgs {
  let port: number | null = null;
  let host = "127.0.0.1";
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--port") {
      const raw = argv[i + 1];
      i += 1;
      if (!raw) throw new Error("--port requires a value");
      const parsed = Number(raw);
      if (!Number.isInteger(parsed) || parsed <= 0 || parsed > 65535) {
        throw new Error(`--port value ${JSON.stringify(raw)} is not a TCP port`);
      }
      port = parsed;
    } else if (arg === "--host") {
      host = argv[i + 1] ?? "127.0.0.1";
      i += 1;
    } else {
      throw new Error(`unknown argument ${JSON.stringify(arg)}`);
    }
  }
  if (port === null) {
    throw new Error("--port is required (read it from the hbat HBAT_READY line)");
  }
  return { port, host };
}

async function openSocket(host: string, port: number): Promise<Socket> {
  return new Promise((resolveSocket, reject) => {
    const socket = createConnection({ host, port }, () => resolveSocket(socket));
    socket.once("error", reject);
  });
}

async function runHeartbeat(host: string, port: number): Promise<typeof SystemHeartbeatAck.Type> {
  const socket = await openSocket(host, port);
  try {
    const request: typeof SystemHeartbeatRequest.Type = {
      nonce: "spike-" + Date.now().toString(36),
    };
    const payload = encodeBytes(SystemHeartbeatRequest, request);
    const response = await call(socket, SYSTEM_HEARTBEAT.name, payload);
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `heartbeat: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    const ack = decodeBytes(SystemHeartbeatAck, response.output);
    if (ack.nonce !== request.nonce) {
      throw new Error(`heartbeat: nonce mismatch — sent ${request.nonce}, received ${ack.nonce}`);
    }
    return ack;
  } finally {
    socket.end();
  }
}

async function runBankCommit(host: string, port: number): Promise<typeof BankCommitAck.Type> {
  // We commit a SystemHeartbeatRequest event with a known nonce so we
  // can decode it back through the same schema after event.get.
  const heartbeatPayload = encodeBytes(SystemHeartbeatRequest, {
    nonce: "spike-bank-commit",
  });

  // Cast the plain hex string into the branded HexBlob shape that
  // the generated schema expects. Sound at runtime — the brand is a
  // phantom type that the schema validates at encode time anyway.
  const request: typeof BankCommitRequest.Type = {
    entity: "spike:demo",
    scope: "spike-scope",
    kind_category: 15,
    kind_type_id: 2561,
    payload_hex: encodeHex(heartbeatPayload) as typeof BankCommitRequest.Type.payload_hex,
    // Optional durable-idempotency operation key (manifest v2, additive). This
    // spike does not dedupe, so it is left unset (null).
    idempotency_key_hex: null,
  };
  const socket = await openSocket(host, port);
  try {
    const payload = encodeBytes(BankCommitRequest, request);
    const response = await call(socket, BANK_COMMIT.name, payload);
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `bank.commit: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    return decodeBytes(BankCommitAck, response.output);
  } finally {
    socket.end();
  }
}

async function runEventGet(
  host: string,
  port: number,
  eventIdHex: string,
): Promise<typeof EventGetAck.Type> {
  const socket = await openSocket(host, port);
  try {
    const request: typeof EventGetRequest.Type = {
      // Cast plain string into the branded EventIdHex; the schema
      // validates the pattern at encode time.
      event_id_hex: eventIdHex as typeof EventGetRequest.Type.event_id_hex,
    };
    const payload = encodeBytes(EventGetRequest, request);
    const response = await call(socket, EVENT_GET.name, payload);
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `event.get: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    return decodeBytes(EventGetAck, response.output);
  } finally {
    socket.end();
  }
}

async function runEventQuery(host: string, port: number): Promise<typeof EventQueryAck.Type> {
  const socket = await openSocket(host, port);
  try {
    const request: typeof EventQueryRequest.Type = {
      entity: "spike:demo",
      scope: "spike-scope",
      kind_category: 15,
      kind_type_id: 2561,
      after_global_sequence: null,
      limit: 16,
    };
    const payload = encodeBytes(EventQueryRequest, request);
    const response = await call(socket, EVENT_QUERY.name, payload);
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `event.query: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    return decodeBytes(EventQueryAck, response.output);
  } finally {
    socket.end();
  }
}

async function runUnknownOperationPath(
  host: string,
  port: number,
): Promise<{ code: string; message: string }> {
  const socket = await openSocket(host, port);
  try {
    const payload = encodeBytes(SystemHeartbeatRequest, { nonce: "unused" });
    const response = await call(socket, "system.heartbeat.nope", payload);
    if (response.kind !== "netbat-error") {
      throw new Error(
        `unknown_operation: expected ERR, got ${response.kind} (output ${response.output.length} bytes)`,
      );
    }
    return { code: response.code, message: response.message };
  } finally {
    socket.end();
  }
}

async function main(): Promise<void> {
  const { host, port } = parseArgs(process.argv.slice(2));
  console.log(`spike: connecting to ${host}:${port}`);

  // 1. Heartbeat.
  const heartbeat = await runHeartbeat(host, port);
  console.log(
    `spike: system.heartbeat OK { nonce=${JSON.stringify(heartbeat.nonce)}, server_ts_ms=${heartbeat.server_ts_ms} }`,
  );

  // 2. bank.commit.
  const commit = await runBankCommit(host, port);
  console.log(
    `spike: bank.commit OK { event_id_hex=${commit.event_id_hex}, sequence=${commit.sequence} }`,
  );
  if (commit.event_id_hex.length !== 32) {
    throw new Error(`bank.commit: event_id_hex length=${commit.event_id_hex.length}, want 32`);
  }

  // 3. event.query over the coordinate/kind, paged by global sequence.
  const page = await runEventQuery(host, port);
  console.log(
    `spike: event.query OK { entries=${page.entries.length}, truncated=${page.truncated}, next_after_global_sequence=${page.next_after_global_sequence} }`,
  );
  const summary = page.entries.find((entry) => entry.event_id_hex === commit.event_id_hex);
  if (!summary) {
    throw new Error("event.query: did not enumerate the just-committed event");
  }
  if (summary.global_sequence !== commit.sequence) {
    throw new Error(
      `event.query: global_sequence ${summary.global_sequence} did not match bank.commit sequence ${commit.sequence}`,
    );
  }

  // 4. event.get with the id discovered through event.query.
  const event = await runEventGet(host, port, summary.event_id_hex);
  console.log(
    `spike: event.get OK { event_id_hex=${event.event_id_hex}, entity=${event.entity}, scope=${event.scope}, kind=${event.kind_category}/${event.kind_type_id} }`,
  );
  if (event.event_id_hex !== commit.event_id_hex) {
    throw new Error("event.get: event_id_hex mismatch with bank.commit");
  }
  if (event.entity !== "spike:demo" || event.scope !== "spike-scope") {
    throw new Error("event.get: coordinate mismatch with bank.commit");
  }

  // 5. Decode the original payload back through the SystemHeartbeatRequest
  //    schema — proves the bytes round-trip through commit + get + Effect 4.
  const recovered = decodeBytes(
    SystemHeartbeatRequest,
    new Uint8Array(event.payload_hex.match(/.{1,2}/gu)!.map((b) => parseInt(b, 16))),
  );
  if (recovered.nonce !== "spike-bank-commit") {
    throw new Error(`event.get: payload nonce mismatch — got ${recovered.nonce}`);
  }
  console.log(
    `spike: event.get payload decoded back through SystemHeartbeatRequest schema { nonce=${JSON.stringify(recovered.nonce)} }`,
  );

  // 6. Error path.
  const err = await runUnknownOperationPath(host, port);
  console.log(
    `spike: unknown_operation ERR { code=${err.code}, message=${JSON.stringify(err.message)} }`,
  );
  if (err.code !== "unknown_operation") {
    throw new Error(`unknown_operation: expected code=unknown_operation, got ${err.code}`);
  }

  console.log("spike: ok");
}

main().catch((error) => {
  console.error(`spike: ${(error as Error).message}`);
  process.exit(1);
});
