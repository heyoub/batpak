/**
 * Phase 0 live spike. Connects to a running `hbat` on `--port <N>`,
 * sends a `system.heartbeat` CALL with the fixture nonce, decodes the
 * ack, then sends a deliberate unknown-operation CALL and decodes the
 * resulting NETBAT/1 ERR frame.
 *
 * Boot `hbat` separately:
 *
 *   cargo run -p hbat -- serve --store $(mktemp -d) --tcp 127.0.0.1:0 --print-port
 *
 * Parse the `HBAT_READY {"port": N, ...}` line on stdout, then invoke:
 *
 *   pnpm --filter @batpak/example-heartbeat-spike start -- --port N
 */

import { createConnection } from "node:net";

import { decode, encode } from "@batpak/canonical";
import { call } from "@batpak/client";
import {
  SystemHeartbeatAck,
  SystemHeartbeatRequest,
  SYSTEM_HEARTBEAT,
} from "@batpak/generated";

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

async function openSocket(host: string, port: number): Promise<import("node:net").Socket> {
  return new Promise((resolveSocket, reject) => {
    const socket = createConnection({ host, port }, () => resolveSocket(socket));
    socket.once("error", reject);
  });
}

async function runHappyPath(host: string, port: number): Promise<SystemHeartbeatAck> {
  const socket = await openSocket(host, port);
  try {
    const request: SystemHeartbeatRequest = { nonce: "spike-" + Date.now().toString(36) };
    const payload = encode(request);
    const response = await call(socket, SYSTEM_HEARTBEAT.name, payload);
    if (response.kind !== "netbat-ok") {
      throw new Error(
        `heartbeat: expected OK, got ${response.kind} ${response.code}: ${response.message}`,
      );
    }
    const ack = decode(response.output) as SystemHeartbeatAck;
    if (ack.nonce !== request.nonce) {
      throw new Error(
        `heartbeat: nonce mismatch — sent ${request.nonce}, received ${ack.nonce}`,
      );
    }
    if (!Number.isSafeInteger(ack.server_ts_ms)) {
      throw new Error(
        `heartbeat: server_ts_ms ${ack.server_ts_ms} exceeds Number.MAX_SAFE_INTEGER`,
      );
    }
    return ack;
  } finally {
    socket.end();
  }
}

async function runErrorFixturePath(
  host: string,
  port: number,
): Promise<{ code: string; message: string }> {
  const socket = await openSocket(host, port);
  try {
    const errorFixturePayload = encode({ nonce: "unused" });
    const response = await call(
      socket,
      "system.heartbeat.nope",
      errorFixturePayload,
    );
    if (response.kind !== "netbat-error") {
      throw new Error(
        `errorFixture: expected ERR, got ${response.kind} (OK output ${response.output.length} bytes)`,
      );
    }
    return { code: response.code, message: response.message };
  } finally {
    socket.end();
  }
}

async function main(): Promise<void> {
  const { host, port } = parseArgs(process.argv.slice(2));
  console.log(`heartbeat-spike: connecting to ${host}:${port}`);

  const ack = await runHappyPath(host, port);
  console.log(
    `heartbeat-spike: OK SystemHeartbeatAck { nonce=${JSON.stringify(ack.nonce)}, server_ts_ms=${ack.server_ts_ms} }`,
  );

  const err = await runErrorFixturePath(host, port);
  console.log(
    `heartbeat-spike: ERR NetbatError { code=${JSON.stringify(err.code)}, message=${JSON.stringify(err.message)} }`,
  );

  if (err.code !== "unknown_operation") {
    throw new Error(
      `heartbeat-spike: error fixture expected unknown_operation, got ${err.code}`,
    );
  }
  console.log("heartbeat-spike: ok");
}

main().catch((error) => {
  console.error(`heartbeat-spike: ${(error as Error).message}`);
  process.exit(1);
});
